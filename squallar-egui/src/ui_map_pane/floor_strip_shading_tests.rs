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
//! **Why the evidence is here and not in the app harness.** Not because the
//! answer cannot be forced -- `ui_map::test_ground_fields` stands the missing
//! scheduler in and the Base Map inspector's inert switch is proven through
//! the real chrome that way -- but because **no floor strip is observable
//! through the app harness at all**: nothing pushes the strip's paint order,
//! so there is no probe to read whichever answer the pane was given (see
//! [`dispatched`]). `render_pane_map_content` is the seam the suppression
//! lives at and the seam both answers are constructible at, so it is where
//! both directions are asserted.

use super::*;
use crate::pane::PaneState;
use squallar_overlays::render::overlay_state::OverlayRegistry;

/// A pane whose ground IS a mesh, spelled the only way the type allows: by
/// holding the height field the ground would be drawn from.
///
/// There is no shortcut here on purpose. `GroundIsMesh`'s field is private to
/// `pane_render`, so `GroundIsMesh(true)` does not compile from anywhere a
/// caller lives, and the fixture has to build the real carrier -- which is the
/// same thing production will hand it once a scheduler exists.
fn a_field() -> GroundIsMesh {
    let field = crate::volume_view::GroundHeightField {
        id: 1,
        site: (35.3331, -97.2778),
        x_km: (-40.0, 40.0),
        y_km: (-40.0, 40.0),
        posts: [2, 2],
        samples: std::sync::Arc::new(vec![0, 1, 2, 3]),
        base_m: 300.0,
        quantum_m: 1.0,
        range_m: (300.0, 303.0),
    };
    let ground = GroundIsMesh::from_height_field(Some(&field));
    assert_ne!(
        ground,
        GroundIsMesh::PLAN_VIEW,
        "fixture: a held field must answer differently from a plan view, or \
         every row below is comparing one state with itself"
    );
    ground
}

/// What one call to [`render_pane_map_content`] dispatched, in paint order.
///
/// **Read off the `PaneRenderCtx` directly, because no floor strip has ever
/// been observable through the app harness.** `last_paint_order` is pushed at
/// exactly one site, the `GroundAndGlass` arm; `draw_floor_strip` never
/// pushes, and the strip's own `ctx.paint_order` is dropped with the closure.
/// So there is no probe to drive a strip through even if
/// `pane_ground_heights` could answer -- which is the second, independent
/// reason the wiring below is pinned in source rather than exercised.
fn dispatched(surfaces: PaneSurfaces, ground: GroundIsMesh) -> Vec<LayerId> {
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
        egui::Id::new(("floor_strip_shading", ground == GroundIsMesh::PLAN_VIEW)),
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
        galley_cache: &mut walkers::GalleyCache::default(),
        point_text_meshes: &mut crate::point_painter::PointTextMeshes::default(),
        ground_meshes: None,
        // No tile source either way: what is being measured is which arms the
        // walk dispatches, which is what the paint-order record carries, and
        // an arm that runs with nothing to draw still records itself.
        basemap_tiles: None,
        terrain_tiles: None,
        tile_zoom_bias: 0,
        overlay_render_limit: 1,
        overlay_overdraw: crate::overlay_cache::OVERDRAW_FRACTION,
        actions: &mut actions,
        pane_rect: canvas,
        surfaces,
        draws_3d_ground: ground,
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
    let mesh = dispatched(PaneSurfaces::GroundOnly, a_field());
    let flat = dispatched(PaneSurfaces::GroundOnly, GroundIsMesh::PLAN_VIEW);

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
    let two_d = dispatched(PaneSurfaces::GroundAndGlass, a_field());
    let strip = dispatched(PaneSurfaces::GroundOnly, a_field());

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

/// Every production module of `ui::map`, which is exactly the set that can
/// construct a [`PaneSurfaces`] or a [`GroundIsMesh`].
///
/// **The set matters, and an earlier version of this pin got it wrong** by
/// reading `ui_map.rs` alone. Both types are `pub(super)` in
/// `ui::map::pane_render`, and `pub(super)` visibility is inherited by every
/// descendant of `ui::map` -- so a second floor strip in `ui_section_pane.rs`
/// or `ui_volume_alpha.rs` would have sailed past a pin whose own message
/// said "a second floor strip appeared".
const MAP_MODULES: &[(&str, &str)] = &[
    (
        "ui_map.rs",
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/ui_map.rs")),
    ),
    (
        "ui_section_pane.rs",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/ui_section_pane.rs"
        )),
    ),
    (
        "ui_volume_alpha.rs",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/ui_volume_alpha.rs"
        )),
    ),
];

/// How many times `needle` occurs across every production module of `ui::map`.
fn across_map_modules(needle: &str) -> usize {
    MAP_MODULES
        .iter()
        .map(|(_, src)| squeezed(src).matches(needle).count())
        .sum()
}

/// Source with every run of whitespace collapsed to one space, so a needle
/// spanning a line break matches whatever column rustfmt chose to wrap at.
/// A pin that a re-wrap can redden is a maintenance trap, not a gate.
fn squeezed(src: &str) -> String {
    src.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// **The wiring, pinned in source, because it cannot be pinned in behaviour
/// and this unit's other two tests cannot see it.**
///
/// The strip is not observable through the app harness at all -- see
/// [`dispatched`] -- so no test that drives the app can read which flag the
/// strip was handed, whatever `pane_ground_heights` answers. Closing this
/// behaviourally still needs an edit this unit does not own: a probe on the
/// strip's own paint order.
///
/// **What it holds, and what a previous version of it did not.** The first
/// spelling asserted only that the pinned line *existed*, which a mutant
/// walked straight through: one added line,
/// `let draws_3d_ground = draws_3d_ground || pane.volume().is_some();`, made
/// every 3D pane's strip drop its hillshade while `pane_ground_heights` stayed
/// `None` and no mesh ever drew -- a permanently unshaded floor on every 3D
/// pane -- and `cargo test --workspace` stayed at 5039 passed / 0 failed,
/// byte-identical to the control. Checker and checked shared the belief "the
/// text is there, so the wiring is there". So this pin counts **bindings and
/// initialisers**, not the presence of a line: a second producer of the name
/// is what the mutant needs and what the counts refuse.
///
/// It is the weaker half of a pair. The stronger half is [`GroundIsMesh`]
/// itself, whose private field means that mutant no longer typechecks; this
/// catches the mutations that stay well-typed, chiefly a strip hardcoded to
/// `PLAN_VIEW`, which would silently disable the suppression for ever the day
/// a scheduler lands.
///
/// **Delete it the day the strip gains a probe on its own paint order**,
/// because from that day the join is reachable behaviourally and a source pin
/// is the weaker statement. The scheduler stand-in is not enough on its own:
/// it can put a pane on the mesh side, but nothing can then read what the
/// strip drew.
#[test]
fn the_strips_flag_and_the_renderers_heights_are_read_from_one_function() {
    let ui_map = squeezed(MAP_MODULES[0].1);
    let ui_map = ui_map.as_str();

    // The pin is reading the files it thinks it is.
    assert!(
        ui_map.contains("fn draw_floor_strip") && ui_map.contains("fn volume_pane_outcome"),
        "this pin is not looking at the module that draws the floor strip"
    );

    assert_eq!(
        across_map_modules("PaneSurfaces::GroundOnly"),
        1,
        "a second floor strip appeared somewhere in ui::map; it needs its own \
         answer to whether its pane's ground is a mesh, and this pin needs to \
         know about it"
    );
    assert_eq!(
        across_map_modules("PaneSurfaces::GroundAndGlass"),
        1,
        "a second plan-view walk appeared in ui::map; the counts below no \
         longer say which one is handed PLAN_VIEW"
    );

    // **One producer of the name, one consumer, one plan-view answer.** These
    // three counts are what the earlier pin lacked: a mutant that composes the
    // answer out of a second belief has to bind the name twice, and a mutant
    // that hardcodes the strip has to move the initialiser.
    assert_eq!(
        across_map_modules("draws_3d_ground ="),
        1,
        "`draws_3d_ground` is bound more than once in ui::map. A second \
         binding is how the answer stops being `pane_ground_heights`'s and \
         starts being the caller's -- the exact mutation that unshaded every \
         3D floor with the whole board green"
    );
    assert_eq!(
        across_map_modules("draws_3d_ground,"),
        1,
        "the floor strip's `PaneRenderCtx` must take the bound value by \
         shorthand; an explicit initialiser here can say something the binding \
         does not"
    );
    assert_eq!(
        across_map_modules("draws_3d_ground:"),
        1,
        "exactly one call site names the field explicitly, and it is the \
         plan-view walk asserted below"
    );

    // **The one expression moved into a named seam, and that is the whole
    // point of it.** The Base Map inspector's "Terrain shading" switch has to
    // ask the same question the strip asks -- it goes inert exactly when the
    // strip drops the layer -- and it lives outside `ui::map`, where
    // `GroundIsMesh`'s constructors are not visible. So the expression is now
    // `pane_draws_3d_ground`'s body, the strip calls it, and the two counts
    // below are unchanged because the seam did not add a reader of
    // `pane_ground_heights`, it renamed the one the strip already was.
    assert_eq!(
        ui_map
            .matches(
                "pub(in crate::ui) fn pane_draws_3d_ground( pane: &crate::pane::PaneState, pane_idx: usize, ) -> pane_render::GroundIsMesh { pane_render::GroundIsMesh::from_height_field(pane_ground_heights(pane, pane_idx).as_deref()) }"
            )
            .count(),
        1,
        "the seam must ask `pane_ground_heights` and nothing else, in one \
         expression with no room between the call and the flag"
    );
    assert_eq!(
        ui_map
            .matches("let draws_3d_ground = pane_draws_3d_ground(pane, pane_idx);")
            .count(),
        1,
        "the floor strip must take its flag off the seam, not compose one"
    );
    assert_eq!(
        ui_map
            .matches("draws_3d_ground: pane_render::GroundIsMesh::PLAN_VIEW,")
            .count(),
        1,
        "the plan-view walk is PLAN_VIEW by construction and says so in one place"
    );
    assert_eq!(
        ui_map
            .matches("heights: pane_ground_heights(pane, pane_idx),")
            .count(),
        1,
        "the renderer's height field must come off the SAME function the strip \
         asked, or the drape and the mesh can disagree about which ground is \
         being drawn"
    );

    // The non-triviality half of a source pin: the needles are ones that can
    // be absent. A pin whose every needle is a substring of itself proves
    // nothing.
    assert_eq!(
        across_map_modules("GroundIsMesh::from_height_field"),
        1,
        "exactly one production site turns a height field into the strip's answer"
    );
    assert_eq!(
        across_map_modules("pane_ground_heights(pane, pane_idx)"),
        2,
        "two readers of the one function -- the strip and the renderer -- and \
         no third"
    );
}
