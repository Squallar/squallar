//! **A pane says what it draws, once, and again only when that moves.**
//!
//! The line is the console's answer to "radar rendered and never appeared":
//! the enabled layers bottom to top, each at its opacity, and a clause when
//! radar sits beneath an opaque full-extent raster. `draw_order_line` is the
//! words over a resolved list; the walk fixture below drives the trigger --
//! the digest the walk folds -- through `render_pane_map_content` itself, so
//! what is held is the shipped instrument and not a fixture of it.

use super::*;
use crate::pane::PaneState;
use squallar_overlays::render::overlay_state::OverlayRegistry;

// ── The words ───────────────────────────────────────────────────────────────

/// Every enabled layer, in order, with its opacity to two places, and no
/// clause when radar is on top of the gridded rasters -- which is where the
/// default weights put it.
#[test]
fn the_line_lists_every_drawn_layer_with_its_opacity_in_order() {
    let drawn = [
        (&known::BASEMAP_TILES, 1.0),
        (&known::GMGSI, 0.85),
        (&known::RADAR, 1.0),
        (&known::NWS_ALERTS, 0.5),
    ];
    assert_eq!(
        draw_order_line(2, &drawn),
        "pane 2 draws: BasemapTiles (op 1.00), Gmgsi (op 0.85), Radar (op 1.00), \
         NwsAlerts (op 0.50)"
    );
}

/// The clause, for each of the three gridded layers, when it is above radar
/// at or over the opaque threshold -- and for none of them below it or
/// beneath radar. `NwsAlerts` above radar at 1.0 is the control: not
/// gridded, so opaque or not it never earns the clause.
#[test]
fn radar_beneath_an_opaque_gridded_raster_is_said_and_nothing_else_is() {
    for gridded in [&known::MRMS, &known::GMGSI, &known::MODEL_DATA] {
        let above_opaque = [(&known::RADAR, 1.0), (gridded, OPAQUE_OVER_RADAR)];
        let line = draw_order_line(0, &above_opaque);
        assert!(
            line.ends_with(&format!(
                "; radar is beneath an opaque {}",
                gridded.as_str()
            )),
            "{line}"
        );

        let above_translucent = [(&known::RADAR, 1.0), (gridded, OPAQUE_OVER_RADAR - 0.01)];
        let line = draw_order_line(0, &above_translucent);
        assert!(!line.contains("beneath"), "{line}");

        let beneath = [(gridded, 1.0), (&known::RADAR, 1.0)];
        let line = draw_order_line(0, &beneath);
        assert!(!line.contains("beneath"), "{line}");
    }
    let control = [(&known::RADAR, 1.0), (&known::NWS_ALERTS, 1.0)];
    let line = draw_order_line(0, &control);
    assert!(!line.contains("beneath"), "{line}");

    // Two opaque rasters above radar are two clauses, not one.
    let two = [
        (&known::RADAR, 1.0),
        (&known::MRMS, 1.0),
        (&known::GMGSI, 1.0),
    ];
    let line = draw_order_line(0, &two);
    assert_eq!(line.matches("beneath an opaque").count(), 2, "{line}");
}

/// A pane drawing nothing still says so, and every line is ASCII.
#[test]
fn an_empty_stack_is_said_and_the_lines_are_ascii() {
    let empty = draw_order_line(4, &[]);
    assert_eq!(
        empty,
        "pane 4 draws: nothing (no enabled layer has a handler in this build)"
    );
    let full = draw_order_line(
        0,
        &[
            (&known::RADAR, 0.333),
            (&known::MRMS, 1.0),
            (&known::CITY_LABELS, 0.0),
        ],
    );
    assert!(empty.is_ascii() && full.is_ascii(), "{full}");
}

// ── The trigger ─────────────────────────────────────────────────────────────

/// One walk over `pane`, as production drives it: the plan-view pass at
/// full opacity with nothing else in the frame.
fn walk(pane: &mut PaneState, overlays: &mut OverlayRegistry) {
    let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
    let egui_ctx = egui::Context::default();
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
        egui::Id::new("draw_order_line_walk"),
        egui::UiBuilder::new()
            .layer_id(egui::LayerId::background())
            .max_rect(canvas),
    );
    let budget = std::cell::Cell::new(u64::MAX);
    let mut ctx = PaneRenderCtx {
        admission_notice: None,
        cost: None,
        pane_idx: 0,
        pane,
        overlays,
        user_location: None,
        user_heading: None,
        user_fix: None,
        basemap_labels: Vec::new(),
        galley_cache: &mut walkers::GalleyCache::default(),
        point_text_meshes: &mut crate::point_painter::PointTextMeshes::default(),
        label_cache: &mut crate::label_cache::LabelCache::default(),
        ground_meshes: None,
        radar_fan: None,
        basemap_tiles: None,
        terrain_tiles: None,
        tile_zoom_bias: 0,
        overlay_render_limit: 1,
        overlay_dispatch_budget: &budget,
        overlay_overdraw: crate::overlay_cache::OVERDRAW_FRACTION,
        actions: &mut actions,
        pane_rect: canvas,
        surfaces: PaneSurfaces::GroundAndGlass,
        draws_3d_ground: GroundIsMesh::PLAN_VIEW,
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
    let _ = egui_ctx.end_pass();
}

/// A pane with the plan-view fixture's stack, hydrated against the real
/// registry.
fn fixture() -> (PaneState, OverlayRegistry) {
    let overlays = OverlayRegistry::with_handlers(crate::sources::all());
    let mut pane = PaneState::new();
    for id in [
        known::BASEMAP_TILES,
        known::RADAR,
        known::CITY_LABELS,
        known::RADAR_SITES,
    ] {
        pane.set_overlay_enabled(id, true);
    }
    pane.hydrate_layer_states(&overlays, 0);
    (pane, overlays)
}

/// **Said on the first walk; unchanged by the next; moved by a toggle, a
/// reorder and an opacity change; and unmoved by a change that draws the
/// same picture.**
///
/// The digest is the trigger and the store beside the `log::info!`, so it
/// is what a test can read. TAMPER: fold the digest after the surface skip
/// and a 3D pane's strip pass re-says the stack every other pass -- not
/// visible here, which is why the fold's position is stated in the walk
/// itself; drop the compare and every frame says it, visible here as the
/// third assertion's `assert_eq` going... nowhere, so the presence control
/// is the moves that follow.
#[test]
fn the_stack_is_said_once_and_again_only_when_it_moves() {
    let (mut pane, mut overlays) = fixture();
    assert_eq!(pane.draw_order_said, 0, "a fresh pane has said nothing");

    walk(&mut pane, &mut overlays);
    let first = pane.draw_order_said;
    assert_ne!(first, 0, "the first walk never said the stack");

    walk(&mut pane, &mut overlays);
    assert_eq!(
        pane.draw_order_said, first,
        "a second walk over the same stack moved the digest: the line is per frame"
    );

    // A toggle moves it.
    pane.set_overlay_enabled(known::NWS_ALERTS, true);
    walk(&mut pane, &mut overlays);
    let toggled = pane.draw_order_said;
    assert_ne!(toggled, first, "enabling a layer did not move the digest");

    // An opacity change moves it.
    pane.set_layer_opacity(&known::RADAR, 0.5);
    walk(&mut pane, &mut overlays);
    let dimmed = pane.draw_order_said;
    assert_ne!(dimmed, toggled, "dimming a layer did not move the digest");

    // A reorder moves it.
    let mut reversed = pane.draw_order_vec();
    reversed.reverse();
    pane.set_draw_order(&reversed);
    walk(&mut pane, &mut overlays);
    let reordered = pane.draw_order_said;
    assert_ne!(reordered, dimmed, "reordering did not move the digest");

    // A disabled layer's opacity is not part of the picture: dimming it
    // moves nothing.
    assert!(!pane.is_overlay_enabled(&known::METAR));
    pane.set_layer_opacity(&known::METAR, 0.1);
    walk(&mut pane, &mut overlays);
    assert_eq!(
        pane.draw_order_said, reordered,
        "an opacity on a layer the pane does not draw moved the digest"
    );
}

/// The digest itself: order-sensitive, opacity-sensitive, and the seed is
/// never what a fresh pane holds.
#[test]
fn the_digest_reads_order_and_opacity() {
    use stack_digest::{SEED, fold};
    assert_ne!(SEED, 0);
    let ab = fold(fold(SEED, &known::RADAR, 1.0), &known::MRMS, 1.0);
    let ba = fold(fold(SEED, &known::MRMS, 1.0), &known::RADAR, 1.0);
    assert_ne!(ab, ba, "the digest does not read order");
    let dim = fold(fold(SEED, &known::RADAR, 0.5), &known::MRMS, 1.0);
    assert_ne!(ab, dim, "the digest does not read opacity");
}
