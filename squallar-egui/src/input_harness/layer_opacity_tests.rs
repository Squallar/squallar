//! **Per-layer opacity is a paint-time tint** (the GIMP layer slider): what
//! the layer walk's `set_opacity` does to the quads a frame really paints,
//! read off the glass through the same `render_map_pane` a user sees.
//!
//! Three things are pinned here. The tint, and that it is the layer's own: a
//! layer at half opacity paints its quad at `WHITE.gamma_multiply(0.5)` while
//! the layer the walk draws after it, in the same frame, paints `WHITE`. (The
//! restore itself, what the `Ui` is handed back at, is pinned at the seam in
//! `ui_map_pane/layer_opacity_walk_tests.rs`: on the glass, the next arm's
//! own `set_opacity` hides a missing restore.) The no-op arm: a stack whose
//! every slot explicitly holds its own resolved default paints what the same
//! stack paints at those defaults implicitly, quad for quad, text for text,
//! segment for segment -- with a layer moved off its default in the same test
//! as the counterfactual. And the cache: an opacity change asks for no
//! raster, because the value is not in `overlay_cache_token` and never may be
//! -- a slider drag re-rasters nothing.

use super::loop_overlay_draw_tests::{LAYER, raster};
use super::tests::{alert_over, ingest_alerts, land_requested_rasters, rasterizes_requested};
use super::{InputHarness, PaintedImage};
use crate::actions::GuiAction;
use squallar_source::id::{LayerId, known};

/// A second `RenderMode::Texture` layer, registered in every build and drawn
/// through the same generic arm as [`LAYER`].
const SIBLING: LayerId = known::NWS_ALERTS;

const FRAME_DT: f64 = 1.0 / 60.0;

/// Both texture layers on, each with a 1x1 raster of its own in pane 0's live
/// cache, handed back in the order the walk draws them: `(first, second)`,
/// each as the layer and the texture it will paint.
fn two_rasters(h: &mut InputHarness) -> ((LayerId, egui::TextureId), (LayerId, egui::TextureId)) {
    h.gui_mut().enable_overlay_for_test(&LAYER);
    h.gui_mut().enable_overlay_for_test(&SIBLING);
    h.warm_up();

    let order = h.gui().panes()[0].draw_order_vec();
    let at = |id: &LayerId| {
        order
            .iter()
            .position(|o| o == id)
            .unwrap_or_else(|| panic!("{id:?} is not in pane 0's draw order"))
    };
    let (first, second) = if at(&LAYER) < at(&SIBLING) {
        (LAYER, SIBLING)
    } else {
        (SIBLING, LAYER)
    };

    let a = raster(h, "first");
    let a_id = a.texture.id();
    let b = raster(h, "second");
    let b_id = b.texture.id();
    let pane = &mut h.gui_mut().panes_mut()[0];
    pane.overlay_cache_mut(&first).show(a);
    pane.overlay_cache_mut(&second).show(b);
    ((first, a_id), (second, b_id))
}

/// The quads the last frame painted inside pane 0, in paint order.
fn quads(h: &InputHarness) -> Vec<PaintedImage> {
    h.painted_images_in(h.pane_rects()[0])
}

/// **The tint, and that it is the layer's own.** The layer drawn first at
/// half opacity paints its quad at `WHITE.gamma_multiply(0.5)`; the layer
/// drawn after it, at its default, paints `WHITE`. The second half pins that
/// **no layer's factor compounds into the next**: each arm's factor is
/// `base * its own layer`, with `base` the opacity the walk was handed.
///
/// The counterfactual is the *pair* coming apart, not the per-arm read on its
/// own: a walk that read `ui.opacity()` inside each arm and still restored
/// after it is this same walk, because the restore puts `base` back before
/// the next read. What compounds is a per-arm read with **no** restore --
/// then each arm multiplies the last arm's leftover. Which is also why this
/// test cannot stand in for the restore itself: measured, with the restore
/// line removed and `base` still read once before the loop, this test stays
/// green, because the next arm's own `set_opacity` overwrites the leak before
/// anything reaches the glass. The restore is `layer_opacity_walk_tests`' to
/// pin.
#[test]
fn a_layer_at_half_opacity_tints_its_quad_and_the_next_layer_paints_at_full() {
    let mut h = InputHarness::new();
    let ((first, first_tex), (second, second_tex)) = two_rasters(&mut h);
    h.gui_mut().panes_mut()[0].set_layer_opacity(&first, 0.5);
    h.frame_after(FRAME_DT);

    let painted = quads(&h);
    let index_of = |tex: egui::TextureId| {
        painted
            .iter()
            .position(|image| image.texture == tex)
            .unwrap_or_else(|| panic!("the raster {tex:?} was not painted inside pane 0"))
    };
    let (dim, full) = (index_of(first_tex), index_of(second_tex));
    assert!(
        dim < full,
        "fixture: {first:?} did not paint before {second:?}, so the second \
         quad says nothing about the restore",
    );
    assert_eq!(
        painted[dim].tint,
        egui::Color32::WHITE.gamma_multiply(0.5),
        "{first:?} at opacity 0.5 painted its quad at the wrong tint",
    );
    assert_eq!(
        painted[full].tint,
        egui::Color32::WHITE,
        "{second:?}, drawn after a layer at 0.5 and itself at its default, \
         painted dimmed: the walk is compounding one layer's factor into the \
         next arm's",
    );
}

/// **The no-op arm: an explicit value equal to the layer's own default paints
/// what the default paints.** Written against each slot's *resolved* default
/// rather than against 1.0, because 1.0 is not the default — it is only
/// today's default, for today's handlers, and a stack every slot of which is
/// explicitly at some *other* number is a stack this test would have said
/// nothing about while claiming to. The two paths into the resolver, the
/// pane's `Some(value)` and the handler's answer, must agree on the glass for
/// the same number.
///
/// Compared within one harness, against a steady-state control: two quiet
/// frames of the untouched stack must agree first, or a difference after the
/// change could be the frame's and not opacity's. And against a genuine
/// counterfactual: one layer moved **off** its default in the same test must
/// change the picture, or the equality above would hold for a walk that
/// ignored opacity altogether.
#[test]
fn every_slot_explicitly_at_its_own_default_paints_what_an_untouched_stack_paints() {
    let mut h = InputHarness::new();
    let ((first, _), _) = two_rasters(&mut h);
    let snapshot = |h: &InputHarness| {
        let rect = h.pane_rects()[0];
        (
            h.painted_images_in(rect),
            h.painted_text_rects(),
            h.all_segments_in(rect),
        )
    };

    // The chrome's separators are still fading in for a few frames after
    // warm-up; a second of quiet frames lets every animation finish, and the
    // control below is what says it did.
    h.frames_for(60, FRAME_DT);
    let a = snapshot(&h);
    h.frame_after(FRAME_DT);
    let b = snapshot(&h);
    assert_eq!(
        a, b,
        "precondition: two quiet frames of the untouched stack differ, so \
         nothing below can be read as opacity's doing",
    );
    assert!(
        !b.0.is_empty(),
        "non-vacuity: no textured quad was painted inside pane 0"
    );

    // Read every slot's resolved default first: the setter below turns each
    // one into an explicit value, and the ones read after it would be reading
    // back what this loop wrote.
    let ids = h.gui().panes()[0].draw_order_vec();
    let defaults: Vec<(LayerId, f32)> = ids
        .iter()
        .map(|id| {
            (
                id.clone(),
                crate::ui::map::pane_render::resolved_layer_opacity(
                    &h.gui().overlays,
                    0,
                    &h.gui().panes()[0],
                    id,
                ),
            )
        })
        .collect();
    for (id, default) in &defaults {
        h.gui_mut().panes_mut()[0].set_layer_opacity(id, *default);
        assert_eq!(
            h.gui().panes()[0].layer_opacity(id),
            Some(*default),
            "fixture: {id:?} took no explicit value",
        );
    }
    h.frame_after(FRAME_DT);
    let c = snapshot(&h);
    assert_eq!(
        c, b,
        "a stack whose every slot explicitly holds its own default painted \
         something the same stack painted at that default implicitly",
    );

    // The counterfactual, in the same test and on the same fixture: a layer
    // moved OFF its default must paint differently. Without it the equality
    // above is satisfied by a walk that never reads the value at all.
    let default_of_first = defaults
        .iter()
        .find(|(id, _)| id == &first)
        .map(|(_, value)| *value)
        .unwrap_or_else(|| panic!("{first:?} is not in pane 0's draw order"));
    let moved = if default_of_first > 0.5 { 0.2 } else { 0.9 };
    assert!(
        (moved - default_of_first).abs() > 0.1,
        "fixture: {first:?}'s default {default_of_first} is too close to \
         {moved} for the change below to be visible",
    );
    h.gui_mut().panes_mut()[0].set_layer_opacity(&first, moved);
    h.frame_after(FRAME_DT);
    assert_ne!(
        snapshot(&h),
        c,
        "{first:?} moved from {default_of_first} to {moved} and the frame \
         painted the same picture: the walk is not reading the value",
    );
}

/// **An opacity change asks for no raster, and still reaches the glass.**
/// The value is a painter tint and is not in `overlay_cache_token`; a token
/// that carried it would re-raster the layer on every notch of a slider
/// drag. The property, not a clock: the frame after the change emits no
/// `RenderOverlay` at all, while the quad it paints carries the new tint.
#[test]
fn an_opacity_change_asks_for_no_raster_and_still_reaches_the_glass() {
    let mut h = InputHarness::new();
    h.gui_mut().enable_overlay_for_test(&SIBLING);
    h.warm_up();
    let ground = h.ground_at(0, h.pane_rects()[0].center());
    ingest_alerts(
        &mut h,
        vec![alert_over("a", "Tornado Warning", ground.y(), ground.x())],
    );
    h.frame_after(FRAME_DT);
    assert!(
        rasterizes_requested(&h, &SIBLING) >= 1,
        "non-vacuity: the alert never asked for a raster, so there is no \
         pipeline here for the change below to leave alone",
    );
    land_requested_rasters(&mut h, &SIBLING);
    h.frames_for(3, FRAME_DT);

    let asks = |h: &InputHarness| {
        h.last_actions()
            .iter()
            .filter(|a| matches!(a, GuiAction::RenderOverlay { .. }))
            .count()
    };
    assert_eq!(
        asks(&h),
        0,
        "control: a landed raster still re-asked, so a quiet frame is not \
         quiet here and the assertion below cannot be read",
    );
    assert!(
        quads(&h)
            .iter()
            .any(|image| image.tint == egui::Color32::WHITE),
        "control: the landed raster is not on the glass",
    );

    h.gui_mut().panes_mut()[0].set_layer_opacity(&SIBLING, 0.37);
    h.frame_after(FRAME_DT);
    assert_eq!(
        asks(&h),
        0,
        "an opacity change asked for a raster: the value has reached the \
         cache token, and a slider drag now re-rasters",
    );
    assert!(
        quads(&h)
            .iter()
            .any(|image| image.tint == egui::Color32::WHITE.gamma_multiply(0.37)),
        "the change asked for nothing, and painted nothing different either",
    );
    h.frame_after(FRAME_DT);
    assert_eq!(
        asks(&h),
        0,
        "the frame after the change asked for a raster the change itself \
         did not",
    );
}
