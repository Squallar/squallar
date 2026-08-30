//! **One ground, one sun.**
//!
//! A 3D pane's floor strip is not a picture anybody looks at: it is the
//! texture the ground mesh is draped with. The hillshade layer is shading
//! already baked into pixels by a sun that lives in map space and does not
//! move when the camera orbits. Draped over a mesh the scene's own light
//! shades, it is a second shadow from a second sun, and the terrain reads as
//! two pictures composited.
//!
//! So the strip of a pane whose ground is a mesh skips `known::TERRAIN`, and
//! nothing else about it changes -- least of all what a 2D pane draws.
//!
//! **Why the evidence is here and not in the app harness.** The answer to
//! "does this pane draw 3D ground" is `ui_map::pane_ground_heights`, and it is
//! `None` for every pane in the shipped build: the height archive B3's path
//! reads is not published, so no scheduler asks for a field. Driving the app
//! can therefore only ever exercise the *false* arm, which is exactly the
//! vacuous half of this test. `render_pane_map_content` is the seam where both
//! answers are constructible, and it is the seam the suppression lives at, so
//! it is where both directions are asserted.

use super::*;
use crate::pane::PaneState;
use squallar_overlays::render::overlay_state::OverlayRegistry;

/// What one call to [`render_pane_map_content`] dispatched, in paint order --
/// the same record `ui_map.rs` pushes into its `last_paint_order` probe.
fn dispatched(surfaces: PaneSurfaces, draws_3d_ground: bool) -> Vec<LayerId> {
    let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
    let egui_ctx = egui::Context::default();

    let mut overlays = OverlayRegistry::with_handlers(crate::sources::all());
    let mut pane = PaneState::new();
    // The layer under test has to be ON, or every arm of this test agrees for
    // the wrong reason. `set_overlay_enabled` before the hydrate, so the state
    // the hydrate mints carries the flag rather than overwriting it with the
    // handler's OFF-by-default. Base tiles first, so the fixture's stack reads
    // bottom-to-top the way the registry's weights order the real one: unlit
    // colour, then the shading over it.
    pane.set_overlay_enabled(known::BASEMAP_TILES, true);
    pane.set_overlay_enabled(known::TERRAIN, true);
    pane.hydrate_layer_states(&overlays, 0);
    assert!(
        pane.is_overlay_enabled(&known::TERRAIN),
        "fixture: Terrain must be on before the walk, or nothing here is measuring the skip"
    );

    let mut memory = walkers::MapMemory::default();
    memory.set_zoom(7.0).expect("7 is a zoom walkers accepts");
    let projector = walkers::Projector::new(canvas, &memory, walkers::lat_lon(35.33, -97.28));

    let mut actions = Vec::new();
    let mut click_consumed = false;
    let preferences = UserPreferences::default();

    egui_ctx.begin_pass(egui::RawInput {
        screen_rect: Some(canvas),
        ..Default::default()
    });
    let mut ui = egui::Ui::new(
        egui_ctx.clone(),
        egui::Id::new(("floor_strip_shading", draws_3d_ground)),
        egui::UiBuilder::new()
            .layer_id(egui::LayerId::background())
            .max_rect(canvas),
    );

    let mut ctx = PaneRenderCtx {
        pane_idx: 0,
        pane: &mut pane,
        overlays: &mut overlays,
        user_location: None,
        user_heading: None,
        user_fix: None,
        basemap_labels: Vec::new(),
        // No tile source either way: what is being measured is which arms the
        // walk dispatches, which is what the paint-order record carries, and
        // an arm that runs with nothing to draw still records itself.
        basemap_tiles: None,
        terrain_tiles: None,
        tile_zoom_bias: 0,
        overlay_render_limit: 1,
        actions: &mut actions,
        pane_rect: canvas,
        surfaces,
        draws_3d_ground,
        horizontal_color_scale: true,
        color_scale_floor: canvas.max.y,
        pointer_available: false,
        excluded_rects: Vec::new(),
        long_press_pos: None,
        overlay_click_pos: None,
        click_consumed: &mut click_consumed,
        preferences: &preferences,
        paint_order: Vec::new(),
    };
    render_pane_map_content(&mut ui, &projector, memory.zoom(), &mut ctx);
    let order: Vec<LayerId> = ctx.paint_order.iter().map(|(id, _)| id.clone()).collect();
    let _ = egui_ctx.end_pass();
    order
}

/// **The suppression, and its non-triviality half, in one run.**
///
/// The two strips differ in one input -- whether the pane's ground is a lit
/// mesh -- and the mesh strip is the one that drops the hillshade. The other
/// strip keeping it is what says the fixture can tell the two states apart at
/// all; the base tiles surviving both is what says the mesh strip dropped one
/// layer rather than went empty.
#[test]
fn the_strip_of_a_pane_whose_ground_is_a_mesh_draws_no_hillshade() {
    let mesh = dispatched(PaneSurfaces::GroundOnly, true);
    let flat = dispatched(PaneSurfaces::GroundOnly, false);

    assert!(
        !mesh.contains(&known::TERRAIN),
        "a 3D pane's strip is the drape on a lit mesh; hillshade in it shades \
         that mesh a second time, under a sun frozen in map space. Dispatched: \
         {mesh:?}"
    );
    assert!(
        flat.contains(&known::TERRAIN),
        "the pane that draws the FLAT map floor has no second light and no \
         other source of shading, so its strip must still carry the hillshade \
         -- and without this half the assertion above is true of every strip. \
         Dispatched: {flat:?}"
    );
    assert!(
        mesh.contains(&known::BASEMAP_TILES),
        "the mesh strip must lose the hillshade and NOTHING else: the base \
         tiles under it are unlit colour and are what the mesh wears. \
         Dispatched: {mesh:?}"
    );
    assert_eq!(
        mesh.len() + 1,
        flat.len(),
        "exactly one layer separates the two strips. Mesh: {mesh:?}, flat: {flat:?}"
    );
}

/// **It cannot reach a 2D pane, whatever a caller passes.**
///
/// The suppression's first conjunct is the pass itself, so a `GroundAndGlass`
/// walk -- the one and only spelling a 2D pane's map content is drawn through
/// -- keeps the hillshade even when handed the flag that suppresses it on a
/// strip. The `GroundOnly` row beside it is the non-triviality half: the same
/// `true` really does suppress somewhere, so this is a guarantee about the
/// surface and not about a flag that never fires.
#[test]
fn a_2d_panes_walk_keeps_the_hillshade_even_under_the_3d_flag() {
    let two_d = dispatched(PaneSurfaces::GroundAndGlass, true);
    let strip = dispatched(PaneSurfaces::GroundOnly, true);

    assert!(
        two_d.contains(&known::TERRAIN),
        "a pane drawing its map in plan has no mesh and no second sun, so the \
         floor strip's suppression must not reach it. Dispatched: {two_d:?}"
    );
    assert!(
        !strip.contains(&known::TERRAIN),
        "fixture: the same flag must actually suppress on the pass it is for, \
         or the row above is asserting that nothing happens. Dispatched: {strip:?}"
    );
}

/// **The wiring, pinned in source, because it cannot be pinned in behaviour.**
///
/// `pane_ground_heights` answers `None` for every pane while the height
/// archive is unpublished, so `draws_3d_ground` is `false` at the strip
/// whether it is read from that function or hardcoded, and the two spellings
/// are indistinguishable to any test that drives the app. That is precisely
/// the state B3 met when it pinned its four production derivation sites by
/// source text rather than by behaviour, and this is the same idiom for the
/// same reason.
///
/// What it holds is the join the two tests above cannot reach: the strip's
/// flag and the renderer's `heights` come from **one** function, and the
/// plan-view walk is handed `false` outright rather than being asked.
///
/// **Delete it the day a scheduler fills `pane_ground_heights` in**, because
/// from that day the join is reachable by driving the app and a source pin is
/// the weaker of the two statements.
#[test]
fn the_strips_flag_and_the_renderers_heights_are_read_from_one_function() {
    const SRC: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/ui_map.rs"));

    // The pin is reading the file it thinks it is.
    assert!(
        SRC.contains("fn draw_floor_strip") && SRC.contains("fn volume_pane_outcome"),
        "this pin is not looking at the module that draws the floor strip"
    );
    assert_eq!(
        SRC.matches("PaneSurfaces::GroundOnly").count(),
        1,
        "a second floor strip appeared; it needs its own answer to whether its \
         pane's ground is a mesh, and this pin needs to know about it"
    );
    assert_eq!(
        SRC.matches("PaneSurfaces::GroundAndGlass").count(),
        1,
        "a second plan-view walk appeared; the pin below no longer says which \
         one is handed `false`"
    );

    assert_eq!(
        SRC.matches("let draws_3d_ground = pane_ground_heights(pane, pane_idx).is_some();")
            .count(),
        1,
        "the floor strip must ask `pane_ground_heights`, not a belief of its own"
    );
    assert_eq!(
        SRC.matches("heights: pane_ground_heights(pane, pane_idx),")
            .count(),
        1,
        "the renderer's height field must come off the SAME function the strip \
         asked, or the drape and the mesh can disagree about which ground is \
         being drawn"
    );
    assert_eq!(
        SRC.matches("draws_3d_ground: false,").count(),
        1,
        "the plan-view walk is `false` by construction and says so in one place"
    );
    assert!(
        !SRC.contains("draws_3d_ground: true"),
        "nothing may hardcode the answer on; it is a question about the pane"
    );
}
