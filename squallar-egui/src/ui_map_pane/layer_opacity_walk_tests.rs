//! **The walk hands the `Ui` back at its own opacity.**
//!
//! The layer walk sets `ui.set_opacity(base * layer)` around each layer's arm
//! and puts `base` back after it. `input_harness/layer_opacity_tests.rs`
//! reads the restore off the painted quads; this reads it off the `Ui`
//! itself, at the seam, where the notices painted after the loop and whatever
//! the caller paints next would otherwise inherit the last layer's tint. The
//! composition too: the `Ui`'s own opacity multiplies into every layer's, and
//! it is the base -- not 1.0 -- that comes back. And the GIMP arm: a layer at
//! 0.0 is still dispatched, and still takes the click, so transparent and
//! switched off keep meaning different things.

use super::*;
use crate::pane::PaneState;
use squallar_overlays::render::overlay_state::OverlayRegistry;

/// What one walk over the fixture is asked to do.
struct Walk<'a> {
    /// The opacity the `Ui` is handed to the walk at.
    base: f32,
    /// One layer set to one opacity, after `every`.
    dimmed: Option<(&'a LayerId, f32)>,
    /// Every slot in the stack set to one opacity, before `dimmed`.
    every: Option<f32>,
    /// Where the frame's overlay click landed, if it had one.
    click: Option<egui::Pos2>,
}

impl Default for Walk<'_> {
    /// A `Ui` at full opacity, nothing dimmed and no click -- which is what
    /// the map content is handed everywhere in production today.
    fn default() -> Self {
        Self {
            base: 1.0,
            dimmed: None,
            every: None,
            click: None,
        }
    }
}

/// What the walk did.
struct Walked {
    /// The `Ui`'s opacity when the walk handed it back.
    after: f32,
    /// Which layers it dispatched, in paint order.
    order: Vec<LayerId>,
    /// Every colour the pass tessellated to, in submission order.
    colors: Vec<egui::Color32>,
    /// Whether an arm took the frame's click for itself.
    click_consumed: bool,
    /// What the arms asked the app for.
    actions: Vec<GuiAction>,
}

/// [`walk_at`] at every default: the `Ui` at 1.0 with nothing dimmed but
/// `dimmed`.
fn walk(dimmed: Option<(&LayerId, f32)>) -> (f32, Vec<LayerId>) {
    let walked = walk_at(Walk {
        dimmed,
        ..Walk::default()
    });
    (walked.after, walked.order)
}

/// One plan-view walk over a pane, as `run` describes it.
///
/// The fixture is `floor_strip_shading_tests`' `dispatched`, over a stack
/// with a glass layer at the top so the order has a last arm worth dimming.
/// It is centred on KTLX at zoom 7, so the canvas centre lands on that
/// station's marker and the click arm has something to hit.
fn walk_at(run: Walk<'_>) -> Walked {
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
    if let Some(opacity) = run.every {
        for id in pane.draw_order_vec() {
            pane.set_layer_opacity(&id, opacity);
            assert_eq!(
                pane.layer_opacity(&id),
                Some(opacity),
                "fixture: {id:?} is in the draw order with no slot to hold a \
                 value, so this walk's layers are not all at one number"
            );
        }
    }
    if let Some((id, opacity)) = run.dimmed {
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
    // What the walk is handed. Nothing in production dims the map content's
    // own `Ui` today -- every `multiply_opacity` site is chrome -- so this is
    // the only caller there is of the composition the walk does with it.
    ui.set_opacity(run.base);

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
        overlay_click_pos: run.click,
        click_consumed: &mut click_consumed,
        preferences: &preferences,
        paint_order: Vec::new(),
    };
    render_pane_map_content(&mut ui, &projector, memory.zoom(), &mut ctx);
    let order: Vec<LayerId> = ctx.paint_order.iter().map(|(id, _)| id.clone()).collect();
    let after = ui.opacity();
    let full = egui_ctx.end_pass();
    // Tessellated, not read off the shapes: `Painter::add` has already put
    // every colour through `Color32::gamma_multiply`, and one flat list of
    // vertex colours covers the quads, the meshes, the paths and the text
    // without a match arm per `Shape` variant.
    let colors: Vec<egui::Color32> = egui_ctx
        .tessellate(full.shapes, egui_ctx.pixels_per_point())
        .iter()
        .flat_map(|clipped| match &clipped.primitive {
            egui::epaint::Primitive::Mesh(mesh) => {
                mesh.vertices.iter().map(|v| v.color).collect::<Vec<_>>()
            }
            egui::epaint::Primitive::Callback(_) => Vec::new(),
        })
        .collect();
    Walked {
        after,
        order,
        colors,
        click_consumed,
        actions,
    }
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

/// **A layer at 0.0 still answers a click.** egui emits `Noop` for every
/// shape a painter at zero opacity is handed, so the layer paints nothing --
/// and the arm still runs, so its hit-testing is untouched: a fully
/// transparent layer keeps hover and click, as GIMP's does. Transparent is
/// not the eye switched off, and the difference is exactly this.
///
/// Read off a real click rather than off the dispatch list: the list says the
/// arm ran, which is the mechanism, and the click says the mechanism has the
/// effect claimed for it. The subject is the radar-sites layer, whose marker
/// is the fixture's one clickable thing -- the canvas centre is KTLX's
/// station -- and the control is the same click with the layer at full
/// strength.
#[test]
fn a_fully_transparent_layer_still_answers_a_click() {
    let sites = known::RADAR_SITES;
    let hit = |opacity: f32| {
        walk_at(Walk {
            dimmed: Some((&sites, opacity)),
            click: Some(egui::pos2(400.0, 300.0)),
            ..Walk::default()
        })
    };
    let switches = |walked: &Walked| {
        walked
            .actions
            .iter()
            .filter(|a| matches!(a, GuiAction::SwitchRadarSite { .. }))
            .count()
    };

    let opaque = hit(1.0);
    assert!(
        opaque.click_consumed,
        "fixture: the click at the canvas centre hit no station marker even \
         with the layer at full strength, so there is no hit-testing here for \
         transparency to lose",
    );
    assert_eq!(
        switches(&opaque),
        1,
        "fixture: the marker was clicked and no site switch was asked for",
    );

    let clear = hit(0.0);
    assert!(
        clear.order.contains(&sites),
        "the radar-sites layer at opacity 0.0 was skipped: the walk is \
         treating transparent as disabled",
    );
    assert!(
        clear.click_consumed,
        "the radar-sites layer at opacity 0.0 did not take the click: a \
         transparent layer has lost its hit-testing, and the eye switch and \
         the slider have stopped meaning different things",
    );
    assert_eq!(
        switches(&clear),
        switches(&opaque),
        "a transparent station marker asked for a different site switch than \
         the same click on an opaque one",
    );
    assert_eq!(
        clear.after, 1.0,
        "the walk left the Ui at {} after an arm at 0.0",
        clear.after,
    );
}

/// **The walk composes the `Ui`'s own opacity with the layer's, and hands
/// back the one it was given.**
///
/// Nothing in production dims the map content's `Ui` today: every
/// `multiply_opacity` in the tree is chrome, and the two opacity fixtures
/// beside this one both start at 1.0. So `base_opacity` had no caller with
/// anything to say -- dropping the `base_opacity *` term, or spelling the
/// restore `ui.set_opacity(1.0)`, was invisible to every test in the tree.
/// Both are the same rule and both are checked here: `base` multiplies in,
/// and `base` is what comes back out.
///
/// The equality is exact rather than tolerant because the two walks reach the
/// same factor the same way. The walk sets one factor per arm,
/// `base * resolved`, and egui applies `Color32::gamma_multiply` once with
/// it; `0.5 * 0.5` and `1.0 * 0.25` are the same `f32`, so the quantization
/// to `u8` happens once, at the same number, in both.
#[test]
fn the_walk_multiplies_the_ui_s_own_opacity_into_every_layer_and_restores_it() {
    let dimmed_ui = walk_at(Walk {
        base: 0.5,
        every: Some(0.5),
        ..Walk::default()
    });
    assert!(
        !dimmed_ui.order.is_empty(),
        "fixture: the walk dispatched nothing, so no arm read the base"
    );
    assert!(
        !dimmed_ui.colors.is_empty(),
        "non-vacuity: the pass tessellated to no coloured vertex at all"
    );

    // Half of a half. Every layer of the first walk paints at `0.5 * 0.5`;
    // the control reaches the same 0.25 with the base out of it, so a walk
    // that dropped the `base_opacity *` term paints the first at 0.5 and the
    // two lists stop agreeing.
    let control = walk_at(Walk {
        every: Some(0.25),
        ..Walk::default()
    });
    assert_eq!(
        dimmed_ui.colors, control.colors,
        "a Ui at 0.5 with every layer at 0.5 did not paint what a Ui at 1.0 \
         with every layer at 0.25 paints: the walk is not multiplying the \
         Ui's own opacity into each layer's",
    );

    // ...and the two halves really are different pictures, or the equality
    // above would hold for a walk that ignored both numbers.
    let at_a_half = walk_at(Walk {
        every: Some(0.5),
        ..Walk::default()
    });
    assert_ne!(
        at_a_half.colors, dimmed_ui.colors,
        "non-vacuity: 0.25 and 0.5 tessellate to the same colours, so this \
         fixture cannot see a tint",
    );

    // And the restore is `base`, not 1.0. The notices the pane paints after
    // the loop, and whatever the caller paints next, run at what the caller
    // set -- a walk that "restored" to 1.0 would hand a dimmed caller back an
    // undimmed Ui.
    assert_eq!(
        dimmed_ui.after, 0.5,
        "the walk was handed a Ui at 0.5 and gave back one at {}",
        dimmed_ui.after,
    );
}
