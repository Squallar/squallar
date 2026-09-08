//! **What the map's shadowed text costs epaint, per pane, per frame.**
//!
//! Every label on a colour-scale bar — the ticks, the unit title, the fold
//! annotation, the range-folded key — goes through
//! [`super::draw_shadowed_text`], which draws the same glyphs twice: a dark
//! copy one point down and right, then the white ink over it. That is two
//! `Shape::Text`s and it has to stay two, because the shadow is what makes a
//! white label readable over mid-green reflectivity.
//!
//! What it does **not** have to be is two layouts. epaint keys its galley
//! cache by the whole `LayoutJob`, colour included, so a `Painter::text` per
//! colour is two distinct entries — two full layouts, two `String`s from a
//! `&str` the caller already holds, two `Context::fonts` write locks — for one
//! string of glyphs whose metrics do not depend on its colour.
//!
//! **Denominator.** Every figure here is *one pane, one
//! [`super::render_color_scales`]*, at [`crate::sources::all`]'s full registry
//! with every layer enabled and a vertical colour-scale gutter. That is one
//! per plan-view pane per frame, so nothing here is a frame figure and nothing
//! here is a six-pane figure. It is also 13 of the helper's 15 call sites; the
//! other two are [`super::draw_pane_cost`]'s, and they are the same function.
//!
//! **The legends and nothing else, on purpose.** A fixture that drove the
//! whole content walk read five extra white galleys under
//! `cargo test --workspace` and none under a filtered run: `input_harness`
//! installs thirteen stations into the **process-global**
//! `squallar_radar::sites` table under a `Once`, and five of them are inside
//! this camera, so `try_draw_site_label` drew five unshadowed names that
//! belong to a different cut. The colour-scale path reads no process-global
//! state — the field registry is a compile-time table and the overlay
//! registry is built here — so this fixture answers the same whichever way it
//! is selected.
//!
//! **Counted off epaint's own cache**, not off ours:
//! `num_galleys_in_cache` is egui's figure and knows nothing about which route
//! asked, so it counts the two-layout shape and the one-layout shape alike.
//!
//! The block also has a *measuring* half — [`super::color_scale_gutter`], which
//! a pane runs every frame to find out how far in from its own edge the
//! legends reach, and which lays out every threshold of every bar to do it.
//! Its vocabulary is a superset of the drawn one, so the last test here asks
//! the sharpest available question: after the measuring pass, the drawing pass
//! must add **no** layouts at all.

use super::*;
use crate::pane::PaneState;
use squallar_overlays::render::overlay_state::OverlayRegistry;

/// One text the pass put on the glass.
struct DrawnText {
    /// The string, as epaint was handed it.
    text: String,
    /// The point size it was laid out at.
    size: f32,
    /// The colour the tessellator will paint it, which is this shape's own
    /// fallback: the galley itself carries no colour any more.
    fallback: egui::Color32,
    /// Where the galley's top-left landed.
    pos: egui::Pos2,
}

/// What one colour-scale pass cost, and what it drew.
struct LegendPass {
    /// Galleys **epaint** had to lay out and keep over this pass.
    epaint_galleys: usize,
    /// Every `Shape::Text` the pass submitted, in submission order.
    drawn: Vec<DrawnText>,
}

/// The ink half of [`super::draw_shadowed_text`].
const INK: egui::Color32 = egui::Color32::WHITE;

/// The shadow half.
const SHADOW: egui::Color32 = egui::Color32::from_black_alpha(200);

/// One colour-scale pass over a pane with every registered layer switched on.
///
/// The pane is built and hydrated **before** the galley count is taken:
/// hydration is a config load's cost, not a frame's.
fn legend_pass() -> LegendPass {
    let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
    let egui_ctx = egui::Context::default();
    let overlays = OverlayRegistry::with_handlers(crate::sources::all());
    let mut pane = PaneState::new();
    let ids: Vec<LayerId> = overlays.handlers().map(|h| h.id()).collect();
    for id in ids {
        pane.set_overlay_enabled(id, true);
    }
    pane.hydrate_layer_states(&overlays, 0);
    let preferences = UserPreferences::default();

    egui_ctx.begin_pass(egui::RawInput {
        screen_rect: Some(canvas),
        ..Default::default()
    });
    let painter = egui::Painter::new(egui_ctx.clone(), egui::LayerId::background(), canvas);
    let before = egui_ctx.fonts(|f| f.num_galleys_in_cache());
    render_color_scales(&painter, canvas, false, 0, &pane, &overlays, &preferences);
    let epaint_galleys = egui_ctx
        .fonts(|f| f.num_galleys_in_cache())
        .saturating_sub(before);
    let output = egui_ctx.end_pass();

    fn collect(shape: &egui::Shape, into: &mut Vec<DrawnText>) {
        match shape {
            egui::Shape::Text(text) => into.push(DrawnText {
                text: text.galley.job.text.clone(),
                size: text
                    .galley
                    .job
                    .sections
                    .first()
                    .map_or(0.0, |s| s.format.font_id.size),
                fallback: text.fallback_color,
                pos: text.pos,
            }),
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| collect(s, into)),
            _ => {}
        }
    }
    let mut drawn = Vec::new();
    for clipped in &output.shapes {
        collect(&clipped.shape, &mut drawn);
    }
    LegendPass {
        epaint_galleys,
        drawn,
    }
}

/// **One string, one layout — not one per colour.**
///
/// Asserted against the pass's own vocabulary rather than a recorded number,
/// so it stays true as bars are added and removed: the layouts epaint had to
/// keep must be exactly the number of distinct (string, point size) pairs the
/// pass drew. The defect made it twice that, because the shadow's colour and
/// the ink's are two different layout jobs over the same glyphs.
///
/// Measured on this tree at the time of the cut: 88 texts drawn from 28
/// distinct pairs, laid out 56 times before and 28 after.
#[test]
fn a_shadowed_label_is_laid_out_once_and_not_once_per_colour() {
    let pass = legend_pass();

    let mut distinct: Vec<(&str, u32)> = pass
        .drawn
        .iter()
        .map(|d| (d.text.as_str(), d.size.to_bits()))
        .collect();
    distinct.sort_unstable();
    distinct.dedup();

    // Printed whether or not the assertion fires: the figure is the finding.
    eprintln!(
        "one pane, every layer on: {} texts drawn, {} distinct (string, size), \
         {} epaint layouts",
        pass.drawn.len(),
        distinct.len(),
        pass.epaint_galleys,
    );
    assert!(
        distinct.len() > 1,
        "premise: the pass drew {} distinct strings, so a doubling would be \
         invisible in the count below",
        distinct.len(),
    );
    assert_eq!(
        pass.epaint_galleys,
        distinct.len(),
        "the pass drew {} distinct (string, size) pairs and made epaint keep \
         {} layouts. A shadowed label laid out once per colour costs two \
         layouts, two `String`s and two `Context::fonts` write locks for one \
         string of glyphs whose metrics do not depend on its colour.",
        distinct.len(),
        pass.epaint_galleys,
    );
}

/// **The other direction: the shadow is still drawn.**
///
/// Halving the layouts by not drawing the shadow at all would pass the count
/// above and leave every white label on the map unreadable over a lit bar. So
/// this asserts the pair: as many dark copies as white ones, and each dark one
/// exactly [`super::SHADOW_OFFSET`] down and right of an ink draw of the same
/// string.
#[test]
fn every_shadowed_label_still_draws_both_its_copies() {
    let pass = legend_pass();

    let shadows: Vec<&DrawnText> = pass.drawn.iter().filter(|d| d.fallback == SHADOW).collect();
    let inks: Vec<&DrawnText> = pass.drawn.iter().filter(|d| d.fallback == INK).collect();

    assert!(
        !shadows.is_empty(),
        "premise: the pass drew no shadowed text at all, so this gate is \
         asserting nothing",
    );
    assert_eq!(
        shadows.len() + inks.len(),
        pass.drawn.len(),
        "the pass drew text in a colour that is neither the shadow nor the \
         ink, so the two counts below no longer describe every label it wrote",
    );
    assert_eq!(
        shadows.len(),
        inks.len(),
        "{} dark copies against {} white ones: a shadowed label is a pair and \
         the two halves have come apart",
        shadows.len(),
        inks.len(),
    );

    let offset = egui::vec2(SHADOW_OFFSET, SHADOW_OFFSET);
    for shadow in &shadows {
        assert!(
            inks.iter().any(|ink| {
                ink.text == shadow.text
                    && ink.size.to_bits() == shadow.size.to_bits()
                    && (ink.pos + offset - shadow.pos).length() < 0.01
            }),
            "the dark copy of {:?} at {:?} has no white copy one point up and \
             left of it: the shadow and the ink no longer register",
            shadow.text,
            shadow.pos,
        );
    }
}

/// **The measuring half and the drawing half share their galleys.**
///
/// A pane asks [`super::color_scale_gutter`] every frame how far in its
/// legends reach, and that walks the same bars laying out **every** threshold
/// — not the thinned subset the painter draws — plus each bar's unit title.
/// Its vocabulary therefore contains the painter's, and the two run one after
/// the other on the same context in the same frame.
///
/// So the exact question is: how many layouts does the *drawing* pass add on
/// top of the measuring one? Zero, if the two agree that a string's metrics do
/// not depend on its colour. One per distinct drawn string, if either side
/// bakes an ink into its layout job — which is what a measuring pass in
/// `Color32::WHITE` did against a painter drawing in the placeholder, and what
/// a painter drawing in two inks did against either.
///
/// **Vertical on purpose.** A horizontal gutter measures one `"0"` for the row
/// height and no ticks at all (`legend_block_reach`), so the superset relation
/// this rests on only holds on the vertical arm.
///
/// **One string is outside the relation, and this fixture does not draw it.**
/// The range-folded key `RF` is painted beside a velocity or spectrum-width
/// bar and is not measured by the gutter, which reserves the swatch's room
/// from `SCALE_MARGIN` instead. A pane whose default product moved to one of
/// those two would read `1` here — a true report of a real gap in the gutter's
/// vocabulary, not of a colour baked into a layout.
#[test]
fn the_gutter_measures_the_same_galleys_the_painter_draws() {
    let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
    let egui_ctx = egui::Context::default();
    let overlays = OverlayRegistry::with_handlers(crate::sources::all());
    let mut pane = PaneState::new();
    let ids: Vec<LayerId> = overlays.handlers().map(|h| h.id()).collect();
    for id in ids {
        pane.set_overlay_enabled(id, true);
    }
    pane.hydrate_layer_states(&overlays, 0);
    let preferences = UserPreferences::default();

    egui_ctx.begin_pass(egui::RawInput {
        screen_rect: Some(canvas),
        ..Default::default()
    });
    let painter = egui::Painter::new(egui_ctx.clone(), egui::LayerId::background(), canvas);
    let count = || egui_ctx.fonts(|f| f.num_galleys_in_cache());

    let before_gutter = count();
    let gutter = color_scale_gutter(&painter, canvas, false, 0, &pane, &overlays, &preferences);
    let after_gutter = count();
    let measured = after_gutter.saturating_sub(before_gutter);
    render_color_scales(&painter, canvas, false, 0, &pane, &overlays, &preferences);
    let drew = count().saturating_sub(after_gutter);
    let _ = egui_ctx.end_pass();

    // Printed whether or not the assertion fires: the figures are the finding.
    eprintln!(
        "one pane, every layer on, vertical gutter {gutter:.1} pt: \
         {measured} layouts to measure, {drew} more to draw"
    );
    assert!(
        measured > 1,
        "premise: the gutter laid out {measured} strings, so a painter minting \
         its own copies of them would be invisible below",
    );
    assert_eq!(
        drew, 0,
        "the painter added {drew} layouts on top of the {measured} the gutter \
         had already made for the same strings on the same frame. One of the \
         two is baking an ink colour into its layout job, and epaint keys its \
         galley cache by the whole job — colour included.",
    );
}
