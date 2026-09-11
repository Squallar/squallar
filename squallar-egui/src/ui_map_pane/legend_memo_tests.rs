//! **The radar colour bar is a picture that is built once, and rebuilt when
//! anything it is a picture of moves.**
//!
//! [`super::render_color_scale`] holds its finished shapes in
//! [`crate::legend_ramp::painted`] and replays them through one
//! `Painter::extend`. That is worth `~33,000` instructions a frame per pane on
//! the headless harness — a fifth of what the frame's whole text layout costs
//! — and it buys them by not doing work whose answer cannot have changed.
//!
//! Both halves of that are gated here, and the **rebuild** half is the one
//! that matters: a memo that hits when it should not is not a slow legend, it
//! is last frame's legend still on the glass, at last frame's rect, in last
//! frame's units. The cheapest input to move is the pane's own rect, which is
//! also the everyday one — a window resize — and a stale hit there paints the
//! bar outside the pane it belongs to.
//!
//! **Counted through epaint's own eviction.** A galley the cache still holds
//! is handed back to a rebuild and to a replay alike, so pointer identity
//! across two adjacent passes proves nothing. An intervening pass that draws
//! nothing drops every galley nothing used, so a rebuild after one mints new
//! `Galley` allocations while a replay hands back the ones the memo is
//! holding. That is what makes `Arc::ptr_eq` below decisive.

use super::*;
use crate::pane::PaneState;
use squallar_overlays::render::overlay_state::OverlayRegistry;

/// A pane with every registered layer on, hydrated — the fixture
/// [`super::shadowed_text_tests`] draws from, for its reasons.
fn pane_with_every_layer(overlays: &OverlayRegistry) -> PaneState {
    let mut pane = PaneState::new();
    let ids: Vec<LayerId> = overlays.handlers().map(|h| h.id()).collect();
    for id in ids {
        pane.set_overlay_enabled(id, true);
    }
    pane.hydrate_layer_states(overlays, 0);
    pane
}

/// Every galley the radar bar put on the glass in one pass, in paint order.
fn legend_galleys(
    egui_ctx: &egui::Context,
    canvas: egui::Rect,
    bar_rect: egui::Rect,
    pane: &PaneState,
    prefs: &UserPreferences,
) -> Vec<std::sync::Arc<egui::Galley>> {
    egui_ctx.begin_pass(egui::RawInput {
        screen_rect: Some(canvas),
        ..Default::default()
    });
    let painter = egui::Painter::new(egui_ctx.clone(), egui::LayerId::background(), canvas);
    render_color_scale(&painter, bar_rect, false, 0, pane, prefs);
    let output = egui_ctx.end_pass();

    fn collect(shape: &egui::Shape, into: &mut Vec<std::sync::Arc<egui::Galley>>) {
        match shape {
            egui::Shape::Text(text) => into.push(text.galley.clone()),
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| collect(s, into)),
            _ => {}
        }
    }
    let mut galleys = Vec::new();
    for clipped in &output.shapes {
        collect(&clipped.shape, &mut galleys);
    }
    galleys
}

/// One pass that draws nothing, so epaint drops every galley nothing used.
fn evicting_pass(egui_ctx: &egui::Context, canvas: egui::Rect) {
    egui_ctx.begin_pass(egui::RawInput {
        screen_rect: Some(canvas),
        ..Default::default()
    });
    let _ = egui_ctx.end_pass();
}

/// **Built once while nothing moves; built again the moment the rect does.**
#[test]
fn the_legend_is_built_once_until_its_version_moves() {
    let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
    let moved = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(760.0, 600.0));
    let egui_ctx = egui::Context::default();
    let overlays = OverlayRegistry::with_handlers(crate::sources::all());
    let pane = pane_with_every_layer(&overlays);
    let prefs = UserPreferences::default();

    let first = legend_galleys(&egui_ctx, canvas, canvas, &pane, &prefs);
    assert!(
        first.len() > 1,
        "premise: the bar drew {} galleys, so neither half below is asking \
         anything",
        first.len(),
    );

    // Nothing has moved, and the cache the rebuild would have used is gone.
    evicting_pass(&egui_ctx, canvas);
    let replayed = legend_galleys(&egui_ctx, canvas, canvas, &pane, &prefs);
    assert_eq!(
        replayed.len(),
        first.len(),
        "the replay drew a different number of galleys than the build it is \
         supposed to be replaying",
    );
    for (i, (was, now)) in first.iter().zip(&replayed).enumerate() {
        assert!(
            std::sync::Arc::ptr_eq(was, now),
            "galley {i} ({:?}) was laid out again on a frame where nothing \
             the legend is a picture of had changed",
            now.text(),
        );
    }

    // The rect moved: the whole bar is at new coordinates, so a hit here is a
    // legend painted where the pane no longer is.
    evicting_pass(&egui_ctx, canvas);
    let rebuilt = legend_galleys(&egui_ctx, canvas, moved, &pane, &prefs);
    assert!(
        rebuilt
            .iter()
            .zip(&replayed)
            .any(|(now, was)| !std::sync::Arc::ptr_eq(now, was)),
        "the pane's rect moved and every galley came back out of the memo: \
         the bar is being painted at the rect it had before the resize",
    );
}

/// **The bar itself reaches the paint list, on the build and on the replay.**
///
/// Pointer identity above says the text was not laid out again; it says
/// nothing about how many shapes reached the paint list. A pass that handed
/// on a short list — the whole family of one-off errors around a memo that is
/// a `Vec` rather than a value — would satisfy every assertion in the test
/// above, and every assertion in [`super::shadowed_text_tests`] too, because
/// those count *text* and the bar's own fill is not text.
///
/// So this asks the question in geometry the test computes for itself, from
/// the same three constants the painter places the bar with: whatever the
/// pass drew, the fills it drew have to cover the rail the labels are read
/// against. A dropped strip leaves a gap at one end of it; a dropped ramp
/// leaves the whole rail uncovered. Both passes go through the same
/// `Painter::extend`, so a short list fires on the first of them — which is
/// why the first check names itself a premise and says so.
#[test]
fn the_replayed_bar_still_covers_its_rail() {
    let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
    let egui_ctx = egui::Context::default();
    let overlays = OverlayRegistry::with_handlers(crate::sources::all());
    let pane = pane_with_every_layer(&overlays);
    let prefs = UserPreferences::default();

    // `render_color_scale`'s own placement for a vertical bar, restated from
    // the same constants: the rail stands one margin in from the pane's right
    // edge and one up from its bottom.
    let right = canvas.right() - SCALE_MARGIN;
    let bottom = canvas.bottom() - SCALE_MARGIN;
    let bar_length = canvas.height() - SCALE_MARGIN * 2.0 - SCALE_TITLE_MARGIN;
    let rail = egui::Rect::from_min_max(
        egui::pos2(right - SCALE_BAR_WIDTH, bottom - bar_length),
        egui::pos2(right, bottom),
    );

    let filled = |ctx: &egui::Context| -> egui::Rect {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(canvas),
            ..Default::default()
        });
        let painter = egui::Painter::new(ctx.clone(), egui::LayerId::background(), canvas);
        render_color_scale(&painter, canvas, false, 0, &pane, &prefs);
        let output = ctx.end_pass();

        fn walk(shape: &egui::Shape, into: &mut egui::Rect) {
            match shape {
                egui::Shape::Rect(rect) => *into = into.union(rect.rect),
                egui::Shape::Mesh(mesh) => *into = into.union(mesh.calc_bounds()),
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, into)),
                _ => {}
            }
        }
        let mut covered = egui::Rect::NOTHING;
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut covered);
        }
        covered
    };

    let built = filled(&egui_ctx);
    assert!(
        built.contains_rect(rail),
        "premise: the first pass already failed to cover the rail \
         {rail:?} — it filled {built:?}, so the replay below is being \
         compared against a broken build",
    );
    let replayed = filled(&egui_ctx);
    assert!(
        replayed.contains_rect(rail),
        "the replayed bar filled {replayed:?}, which does not cover the rail \
         {rail:?} its labels are read against: the memo handed on a shorter \
         list than it was built with",
    );
}

/// **The overlay bars are the same picture, and were the one half of this
/// file's subject that nothing held to it.**
///
/// [`super::render_overlay_color_scales`] sits one function below
/// [`super::render_color_scale`] and draws the same thing for every
/// legend-carrying layer the pane has on, and until 2026-09-11 it rebuilt
/// every tick galley and took a `Context::graphics` lock per shape on every
/// frame while its twin replayed a held list. It now runs through the same
/// [`crate::legend_ramp::painted`] slot, one per pane and layer.
///
/// Gated exactly as the radar bar above is, and for the same reason: a memo
/// that hits when it should not is last frame's bar, at last frame's rect. The
/// rect is the input moved, because it is the everyday one — a window resize
/// — and every bar in the stack moves with it.
#[test]
fn the_overlay_bars_are_built_once_until_their_version_moves() {
    let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
    let moved = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(760.0, 600.0));
    let egui_ctx = egui::Context::default();
    let overlays = OverlayRegistry::with_handlers(crate::sources::all());
    let pane = pane_with_every_layer(&overlays);

    let galleys = |ctx: &egui::Context, rect: egui::Rect| -> Vec<std::sync::Arc<egui::Galley>> {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(canvas),
            ..Default::default()
        });
        let painter = egui::Painter::new(ctx.clone(), egui::LayerId::background(), canvas);
        render_overlay_color_scales(&painter, rect, false, 0, &pane, &overlays);
        let output = ctx.end_pass();

        fn collect(shape: &egui::Shape, into: &mut Vec<std::sync::Arc<egui::Galley>>) {
            match shape {
                egui::Shape::Text(text) => into.push(text.galley.clone()),
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| collect(s, into)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in &output.shapes {
            collect(&clipped.shape, &mut out);
        }
        out
    };

    let first = galleys(&egui_ctx, canvas);
    assert!(
        first.len() > 1,
        "premise: the overlay stack drew {} galleys, so neither half below is \
         asking anything",
        first.len(),
    );

    evicting_pass(&egui_ctx, canvas);
    let replayed = galleys(&egui_ctx, canvas);
    assert_eq!(
        replayed.len(),
        first.len(),
        "the replay drew a different number of galleys than the build it is \
         supposed to be replaying",
    );
    for (i, (was, now)) in first.iter().zip(&replayed).enumerate() {
        assert!(
            std::sync::Arc::ptr_eq(was, now),
            "galley {i} ({:?}) was laid out again on a frame where nothing \
             the overlay bars are a picture of had changed",
            now.text(),
        );
    }

    evicting_pass(&egui_ctx, canvas);
    let rebuilt = galleys(&egui_ctx, moved);
    assert!(
        rebuilt
            .iter()
            .zip(&replayed)
            .any(|(now, was)| !std::sync::Arc::ptr_eq(now, was)),
        "the pane's rect moved and every overlay galley came back out of the \
         memo: the bars are being painted at the rect they had before the \
         resize",
    );
}
