//! **A switch the pane's own render has superseded goes inert and says what
//! took the work over.**
//!
//! "Terrain shading" turns on the hillshade layer: raster tiles with shaded
//! relief baked into the pixels, lit from a direction frozen in map space. On
//! a pane whose ground is a lit 3D mesh the floor strip refuses to drape it
//! (`ui_map_pane::scene_light_supersedes`), because the mesh already carries
//! the scene's own light and a baked hillshade over it is a second shadow
//! from a second sun. The switch went on doing nothing and saying nothing:
//! with two panes open you would see the hillshade on the 2D one, none on the
//! 3D floor, and one ticked box explaining neither.
//!
//! **Disabled with a note, not hidden.** The toggle IS the pane's persisted
//! terrain state -- the very state the layer's old stack row toggled -- so
//! hiding it would leave that state on the config, still deciding what the
//! pane draws the moment its ground goes flat again, with nothing on screen
//! able to see or reach it. "Reopen is exactly 1:1" makes that worse rather
//! than better.
//!
//! **The fixture problem this unit exists inside.** `pane_ground_heights`
//! answers `None` for every pane in the shipped build, so the enabled path is
//! every pane there is and a test that only walked it would be vacuous for
//! this whole unit. `ui_map::test_ground_fields` stands the missing scheduler
//! in, and every test below runs both arms in one process.
//!
//! **And the third pane is the point.** Every fixture here carries a 3D pane
//! that does *not* draw a mesh -- which is what every 3D pane in the shipped
//! app is -- so a gate spelled `pane.volume().is_some()` reddens rather than
//! passing. Without that row this file would be an identity fixture: two
//! panes that differ in two ways at once, proving neither.

use super::*;
use crate::input_harness::InputHarness;
use crate::terrain::TERRAIN_SUPERSEDED_NOTE;
use squallar_source::id::known;

/// The three panes every fixture below opens, and what each is for.
///
/// The names are the argument. `FLAT_2D` and `MESH_3D` are the two states the
/// feature is about; `BARE_3D` is the uncooperative one -- a 3D pane with no
/// height field, indistinguishable from `MESH_3D` by pane kind and from
/// `FLAT_2D` by ground.
const FLAT_2D: usize = 0;
const BARE_3D: usize = 1;
const MESH_3D: usize = 2;

/// What the **production seam** answers for pane `idx` — the exact expression
/// the Base Map inspector gates on, read here rather than off the lever that
/// set it. A fixture that confirmed itself from its own input would be
/// checker and checked sharing one belief.
fn scene_light_supersedes_on(h: &InputHarness, idx: usize) -> bool {
    let pane = h.gui().pane(idx).unwrap_or_else(|| panic!("no pane {idx}"));
    map::pane_render::scene_light_supersedes(map::pane_draws_3d_ground(pane, idx), &known::TERRAIN)
}

/// Three panes: a map pane, a 3D pane on the flat lid, and a 3D pane whose
/// ground is a mesh. Terrain shading is ON in all three, because a switch
/// asserted in its off state cannot show that being disabled preserved
/// anything.
fn three_panes() -> InputHarness {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(3);
    h.make_pane_volume(BARE_3D);
    h.make_pane_volume(MESH_3D);
    h.set_pane_draws_3d_ground(MESH_3D, true);

    for idx in [FLAT_2D, BARE_3D, MESH_3D] {
        h.set_overlay_on_pane(idx, &known::TERRAIN, true);
    }
    h.warm_up();

    // The fixture is the two states it claims to be, read off the production
    // seam rather than off the lever that set it. Without this the rows below
    // could all be describing one state.
    assert!(
        !scene_light_supersedes_on(&h, FLAT_2D),
        "fixture: the map pane must not be drawing 3D ground"
    );
    assert!(
        !scene_light_supersedes_on(&h, BARE_3D),
        "fixture: a 3D pane with no height field is exactly what every 3D \
         pane in the shipped app is, and it must answer NO -- this is the row \
         that keeps `pane.volume().is_some()` from passing this file"
    );
    assert!(
        scene_light_supersedes_on(&h, MESH_3D),
        "fixture: the mesh pane must be drawing 3D ground, or every \
         assertion below is comparing one state with itself"
    );
    h
}

/// Make pane `idx` active and open the Base Map body, where the one Terrain
/// switch lives (`TerrainHandler::surfaced_through`).
fn open_basemap_on(h: &mut InputHarness, idx: usize) {
    // The user's own route to a pane: click it. The panel underneath must not
    // fade away while we read it, which `warm_up` settles.
    h.mouse_click(h.pane_rects()[idx].center());
    h.warm_up();
    assert_eq!(
        h.active_pane_index(),
        idx,
        "clicking pane {idx} did not make it the active one"
    );
    h.open_layer_in_inspector(&known::BASEMAP_TILES);
}

/// The Terrain switch's rect in the open inspector, scrolled on screen.
///
/// `None` would mean the switch was not drawn at all, which is the shape this
/// decision explicitly refused -- so every caller asserts on it rather than
/// tolerating it.
fn terrain_switch(h: &mut InputHarness) -> egui::Rect {
    let pos = h
        .inspector_rect()
        .expect("the Base Map body is open")
        .center();
    let find = |h: &InputHarness| {
        h.control_items().into_iter().find(|item| {
            item.handler.as_ref() == Some(&known::TERRAIN)
                && item.kind == crate::ui::DrawnControlKind::Checkbox
        })
    };
    let found = h.scroll_until(pos, egui::vec2(0.0, -120.0), 60, |h| {
        find(h).is_some_and(|item| h.screen_rect().contains(item.rect.center()))
    });
    assert!(
        found,
        "the Base Map inspector drew no Terrain shading switch on screen. \
         Disabled is not hidden: a pane whose ground supersedes the layer \
         must still show the switch, or its persisted state is unreachable",
    );
    find(h).expect("the switch was just found").rect
}

/// **Both directions, one run.** The switch is live on the two panes whose
/// ground it still governs and inert on the one whose ground the scene light
/// shades -- and it is *drawn* on all three, because hiding it was the option
/// this decision refused.
#[test]
fn the_terrain_switch_goes_inert_only_where_the_scene_light_shades_the_ground() {
    let mut h = three_panes();

    for (idx, name, superseded) in [
        (FLAT_2D, "the map pane", false),
        (BARE_3D, "a 3D pane on the flat lid", false),
        (MESH_3D, "a 3D pane whose ground is a mesh", true),
    ] {
        open_basemap_on(&mut h, idx);
        let switch = terrain_switch(&mut h);
        let inspector = h.inspector_rect().expect("the Base Map body is open");

        // **The click is the whole evidence.** egui's disabled widgets never
        // report `changed`, so a switch that flips is live and one that does
        // not is inert -- and `terrain_switch` has already proved the widget
        // is on screen under the pointer, which is what stops "nothing
        // happened" meaning "there was nothing there".
        let before = h.overlay_enabled_on(idx, &known::TERRAIN);
        h.mouse_click(switch.center());
        h.warm_up();
        let after = h.overlay_enabled_on(idx, &known::TERRAIN);

        if superseded {
            assert_eq!(
                after, before,
                "{name}: its shading comes from the scene light, so the \
                 switch must not be flippable"
            );
            assert!(
                h.text_painted_in(inspector, TERRAIN_SUPERSEDED_NOTE),
                "{name}: a dead switch with nothing beside it is the bug \
                 this unit is about. The body painted {:?}",
                h.painted_text_strings_in(inspector),
            );
        } else {
            assert_ne!(
                after, before,
                "{name}: nothing supersedes its hillshade, so the switch \
                 must still turn it off"
            );
            assert!(
                !h.text_painted_in(inspector, TERRAIN_SUPERSEDED_NOTE),
                "{name}: told that its shading comes from the scene light \
                 while its own switch is what draws it"
            );
            // Put it back, so the panes stay comparable for any later row.
            h.mouse_click(switch.center());
            h.warm_up();
        }
    }
}

/// **The note names the mechanism in effect rather than apologising**, and
/// that is the whole licence for a caption beside a control the reader cannot
/// operate.
///
/// Pinned as text because the wording IS the decision: a sentence that said
/// "unavailable" or "not supported here" would be a notice with nothing
/// behind it, which this repository refuses. It names the light, and it names
/// it generically -- the pane's own Sunlight control decides whether that
/// light is the real sun or the studio one, and the sentence has to survive
/// either.
#[test]
fn the_note_names_the_light_and_never_apologises() {
    assert_eq!(
        TERRAIN_SUPERSEDED_NOTE,
        "This pane's ground is 3D, and its shading is provided by the scene light.",
    );
    for weasel in [
        "unavailable",
        "not supported",
        "cannot",
        "can't",
        "sorry",
        "disabled",
        "unsupported",
    ] {
        assert!(
            !TERRAIN_SUPERSEDED_NOTE.to_lowercase().contains(weasel),
            "the note apologises ({weasel:?}) instead of naming what is \
             doing the shading: {TERRAIN_SUPERSEDED_NOTE:?}",
        );
    }
}

/// **Being disabled is not being cleared.** Set the switch, put the pane onto
/// 3D ground, save, reopen, go back to flat ground, and the setting is what
/// it was.
///
/// The last leg is the one that matters: a config written while the control
/// was inert must still carry the state, or the day the pane's ground goes
/// flat again -- a camera the scheduler declines to place a field for, a
/// device whose texture limit refuses one -- the hillshade would come back
/// off, silently, having been switched off by nobody.
#[test]
fn terrain_state_set_before_the_switch_went_inert_survives_a_reopen() {
    let mut h = three_panes();

    // Set it through the chrome on a pane whose switch is still live, which
    // is the only way a user could ever have set it.
    open_basemap_on(&mut h, MESH_3D);
    h.set_pane_draws_3d_ground(MESH_3D, false);
    let switch = terrain_switch(&mut h);
    h.mouse_click(switch.center());
    h.warm_up();
    assert!(
        !h.overlay_enabled_on(MESH_3D, &known::TERRAIN),
        "precondition: the click must have turned the shading off"
    );
    // ...and back on, so what persists is a state a click put there rather
    // than the fixture's own opening value.
    h.mouse_click(switch.center());
    h.warm_up();
    assert!(
        h.overlay_enabled_on(MESH_3D, &known::TERRAIN),
        "precondition: the shading is on, set through the switch"
    );

    // Now the ground becomes a mesh and the switch goes inert under it.
    h.set_pane_draws_3d_ground(MESH_3D, true);
    let inspector = h.inspector_rect().expect("the Base Map body is open");
    assert!(
        h.text_painted_in(inspector, TERRAIN_SUPERSEDED_NOTE),
        "precondition: the switch must be inert when the config is written, \
         or this test saves from the state it is trying to leave"
    );

    let store = squallar_kv::MemoryKvStore::default();
    h.gui_mut().save_ui_config(&store);

    let mut reopened = crate::Gui::new();
    assert!(reopened.load_ui_config(&store), "the config must load");
    assert!(
        reopened
            .pane(MESH_3D)
            .expect("the reopened config carries three panes")
            .is_overlay_enabled(&known::TERRAIN),
        "the terrain state was written while its switch was inert and did \
         not come back -- a setting the user made was dropped by a control \
         going quiet",
    );

    // Back to flat ground, where the switch governs again: what it shows is
    // what was set, and it is operable.
    let mut back = InputHarness::new();
    assert!(
        back.gui_mut().load_ui_config(&store),
        "the config must load"
    );
    back.warm_up();
    back.set_pane_draws_3d_ground(MESH_3D, false);
    open_basemap_on(&mut back, MESH_3D);
    let switch = terrain_switch(&mut back);
    let inspector = back.inspector_rect().expect("the Base Map body is open");
    assert!(
        !back.text_painted_in(inspector, TERRAIN_SUPERSEDED_NOTE),
        "the ground is flat again; nothing is superseding the switch"
    );
    assert!(
        back.overlay_enabled_on(MESH_3D, &known::TERRAIN),
        "the shading came back off a reopen it was on for"
    );
    back.mouse_click(switch.center());
    back.warm_up();
    assert!(
        !back.overlay_enabled_on(MESH_3D, &known::TERRAIN),
        "the switch is live again and must turn the restored state off"
    );
}
