//! **That the nine cuts of `panes:content` are nine cuts and not one.**
//!
//! [`crate::shell_api::ContentCuts`] is read as a decomposition — a share of
//! `frame panes (content)` per name — and the two ways that reading goes wrong
//! are both silent. A cut declared and never charged reads as a structural
//! zero indistinguishable from work that did not happen; and a drain that
//! assigned instead of accumulating would keep the last pane's cuts against
//! every pane's parent, which on a one-pane scene is invisible.
//!
//! `frame_ledger`'s `every_content_cut_is_load_bearing_against_the_residual`
//! covers the arithmetic below the charge; these cover the charge itself.

use super::*;
use crate::pane::PaneState;
use crate::shell_api::{ContentCuts, content_ledger};
use squallar_overlays::render::overlay_state::OverlayRegistry;

/// The eight field names [`ContentCuts`] declares, read out of the struct
/// rather than restated — a ninth cut added with no charge site must fail
/// [`every_declared_content_cut_has_a_charge_site`] rather than join it.
fn declared_cuts() -> Vec<String> {
    let source = include_str!("../shell_api.rs");
    let body = source
        .split_once("pub struct ContentCuts {")
        .expect("ContentCuts is no longer declared in shell_api.rs")
        .1
        .split_once("\n}")
        .expect("ContentCuts' declaration has no recognisable end")
        .0;
    body.lines()
        .filter_map(|line| {
            let field = line.trim().strip_prefix("pub ")?.strip_suffix(": u64,")?;
            Some(field.to_string())
        })
        .collect()
}

/// **Every declared cut is charged, and charged exactly where the walk is.**
///
/// The negative half of this — "a field with no charge site fails" — is what
/// the test is for, so the list is read off the struct and not restated here:
/// a restated list passes for free the day a ninth field lands.
#[test]
fn every_declared_content_cut_has_a_charge_site() {
    let walk = include_str!("../ui_map_pane.rs");
    let cuts = declared_cuts();
    assert_eq!(
        cuts.len(),
        9,
        "ContentCuts no longer declares nine cuts ({cuts:?}); the ledger's \
         `content_cut_micros` and `frame_content_lines` are both [_; 10] and \
         must move with it",
    );
    for cut in &cuts {
        let site = format!("&mut cc.{cut}");
        assert!(
            walk.contains(&site),
            "`ContentCuts::{cut}` is declared but never charged in \
             `render_pane_map_content`, so it reports a structural zero that \
             reads as work that did not happen",
        );
    }
}

/// **The drain accumulates.** `render_panes` charges `content_ns` once per
/// pane and drains beside each charge, so a drain that assigned would keep
/// only the last pane's cuts — a bug no one-pane scene can see.
#[test]
fn the_drain_accumulates_across_panes_and_leaves_the_ledger_empty() {
    content_ledger::reset();
    content_ledger::add(ContentCuts {
        prologue_ns: 7,
        items_ns: 11,
        ..ContentCuts::default()
    });
    content_ledger::add(ContentCuts {
        prologue_ns: 5,
        walk_ns: 3,
        ..ContentCuts::default()
    });
    let mut sink = ContentCuts {
        prologue_ns: 100,
        ..ContentCuts::default()
    };
    content_ledger::drain_into(&mut sink);
    assert_eq!(
        sink.prologue_ns, 112,
        "the drain replaced rather than added"
    );
    assert_eq!(sink.items_ns, 11);
    assert_eq!(sink.walk_ns, 3);
    assert_eq!(
        content_ledger::peek(),
        ContentCuts::default(),
        "the drain left a sum behind, so the next frame inherits this one's",
    );
}

/// **A real walk feeds the ledger**, so the drain is not reading an
/// accumulator nothing writes. Asserted on the sum of the eight and not on any
/// one of them: a cut's own figure is a clock reading and this is not a timing
/// test — see `every_declared_content_cut_has_a_charge_site` for which cuts
/// exist.
#[test]
fn a_walk_over_a_real_pane_feeds_the_ledger() {
    let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
    let egui_ctx = egui::Context::default();
    let mut overlays = OverlayRegistry::with_handlers(crate::sources::all());
    let ids: Vec<LayerId> = overlays.handlers().map(|h| h.id()).collect();
    let mut pane = PaneState::new();
    for id in &ids {
        pane.set_overlay_enabled(id.clone(), true);
    }
    pane.hydrate_layer_states(&overlays, 0);

    let mut memory = walkers::MapMemory::default();
    memory.set_zoom(7.0).expect("7 is a zoom walkers accepts");
    let projector = walkers::Projector::new(canvas, &memory, walkers::lat_lon(35.33, -97.28));
    let preferences = squallar_units::UserPreferences::default();
    let mut actions = Vec::new();
    let mut click_consumed = false;
    let mut galley_cache = walkers::GalleyCache::default();
    let mut point_text_meshes = crate::point_painter::PointTextMeshes::default();
    let mut label_cache = crate::label_cache::LabelCache::default();

    egui_ctx.begin_pass(egui::RawInput {
        screen_rect: Some(canvas),
        ..Default::default()
    });
    let mut ui = egui::Ui::new(
        egui_ctx.clone(),
        egui::Id::new("content_cuts"),
        egui::UiBuilder::new()
            .layer_id(egui::LayerId::background())
            .max_rect(canvas),
    );
    let budget = std::cell::Cell::new(u64::MAX);
    content_ledger::reset();
    {
        let mut ctx = PaneRenderCtx {
            admission_notice: None,
            cost: None,
            pane_idx: 0,
            pane: &mut pane,
            overlays: &mut overlays,
            user_location: None,
            user_heading: None,
            user_fix: None,
            basemap_labels: Vec::new(),
            galley_cache: &mut galley_cache,
            point_text_meshes: &mut point_text_meshes,
            label_cache: &mut label_cache,
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
    }
    let cuts = content_ledger::peek();
    let _ = egui_ctx.end_pass();

    let named = [
        cuts.prologue_ns,
        cuts.ground_ns,
        cuts.labels_ns,
        cuts.radar_ns,
        cuts.items_ns,
        cuts.walk_ns,
        cuts.chrome_ns,
        cuts.plates_ns,
        cuts.dispatch_ns,
    ];
    assert!(
        named.iter().any(|ns| *ns > 0),
        "an all-layers walk charged nothing to any of the nine cuts, so the \
         ledger the report reads is an accumulator nothing writes: {cuts:?}",
    );
    content_ledger::reset();
}
