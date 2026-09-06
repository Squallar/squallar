//! **The walk hands the `Ui` back at its own opacity.**
//!
//! The layer walk sets `ui.set_opacity(base * layer)` around each layer's arm
//! and puts `base` back after it. `input_harness/layer_opacity_tests.rs`
//! reads the restore off the painted quads; this reads it off the `Ui`
//! itself, at the seam, where the notices painted after the loop and whatever
//! the caller paints next would otherwise inherit the last layer's tint. And
//! the GIMP arm: a layer at 0.0 is still dispatched, so it keeps its
//! hit-testing.

use super::*;
use crate::pane::PaneState;
use squallar_overlays::render::overlay_state::OverlayRegistry;

/// One plan-view walk over a pane, with `dimmed` set to that opacity on the
/// pane first: the `Ui`'s opacity after the walk, and which layers it
/// dispatched, in paint order. The fixture is `floor_strip_shading_tests`'
/// `dispatched`, over a stack with a glass layer at the top so the order has
/// a last arm worth dimming.
fn walk(dimmed: Option<(&LayerId, f32)>) -> (f32, Vec<LayerId>) {
    let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
    let egui_ctx = egui::Context::default();

    let mut overlays = OverlayRegistry::with_handlers(crate::sources::all());
    let mut pane = PaneState::new();
    for id in [
        known::BASEMAP_TILES,
        known::CITY_LABELS,
        known::RADAR_SITES,
        known::COLOR_SCALE,
    ] {
        pane.set_overlay_enabled(id, true);
    }
    pane.hydrate_layer_states(&overlays, 0);
    if let Some((id, opacity)) = dimmed {
        pane.set_layer_opacity(id, opacity);
        assert_eq!(
            pane.layer_opacity(id),
            Some(opacity),
            "fixture: the pane holds no slot for {id:?}, so nothing was dimmed"
        );
    }

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
        egui::Id::new("layer_opacity_walk"),
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
        basemap_tiles: None,
        terrain_tiles: None,
        tile_zoom_bias: 0,
        overlay_render_limit: 1,
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
    let order: Vec<LayerId> = ctx.paint_order.iter().map(|(id, _)| id.clone()).collect();
    let after = ui.opacity();
    let _ = egui_ctx.end_pass();
    (after, order)
}

/// The last layer the walk dispatches: the one whose arm, dimmed, would leave
/// the `Ui` dimmed if the restore were missing. Any earlier layer's tint is
/// overwritten by the next arm's own `set_opacity` whether or not the walk
/// restores, so it is the last arm that decides.
fn last_dispatched() -> LayerId {
    let (_, order) = walk(None);
    order
        .last()
        .cloned()
        .expect("fixture: the walk dispatched nothing")
}

/// After a walk whose last arm painted at 0.5, the `Ui` is back at 1.0.
#[test]
fn the_walk_hands_the_ui_back_at_its_own_opacity() {
    let last = last_dispatched();
    let (after, order) = walk(Some((&last, 0.5)));
    assert_eq!(
        order.last(),
        Some(&last),
        "fixture: dimming {last:?} moved it out of last place, so this is not \
         the arm that decides",
    );
    assert_eq!(
        after, 1.0,
        "the walk left the Ui at {after}: the last arm's tint leaked past the \
         loop, onto the notices painted after it and whatever the caller \
         paints next",
    );
}

/// A layer at 0.0 is still dispatched. egui emits `Noop` for its shapes,
/// and the arm still runs its hit-testing: a fully transparent layer keeps
/// hover and click, as GIMP's does.
#[test]
fn a_fully_transparent_layer_is_still_dispatched() {
    let last = last_dispatched();
    let (after, order) = walk(Some((&last, 0.0)));
    assert!(
        order.contains(&last),
        "{last:?} at opacity 0.0 was skipped: the walk is treating transparent \
         as disabled, and the layer lost its hover and click",
    );
    assert_eq!(
        after, 1.0,
        "the walk left the Ui at {after} after an arm at 0.0"
    );
}
