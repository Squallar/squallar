//! The Volume Alpha editor: GR2Analyst's drag-editable opacity curve, drawn
//! over the product's own palette strip.
//!
//! One small titled window per 3D pane, opened from a button in the pane's
//! corner and shown *beside* the volume rather than instead of it — the whole
//! point of the tool is watching the storm answer the curve as it is drawn.
//! The window is GR-style: a dark canvas, the white alpha curve across the
//! 256-index value axis, and the palette strip beneath it as the axis labels.
//!
//! # What a drag means
//!
//! Click-drag paints a new curve segment over exactly the value range the
//! pointer crosses — [`crate::volume_alpha::apply_stroke`] per frame of the
//! drag, chained into a freehand line. The rest of the curve is untouched;
//! that is the "per region of the value axis" behaviour, and it is pinned in
//! the model's tests rather than here because it is a property of the stroke,
//! not of the widget.
//!
//! Edits apply **live**: every frame of the drag writes the store the next
//! frame's `VolumeFrameState` reads, and the frontend re-uploads the 1 KiB
//! LUT only when the bytes changed. Right-click on the canvas, or the Reset
//! button, forgets the product's curve entirely — which restores the **grid's
//! own table** bit-exactly, because "no curve" is the bit-exact state.
//!
//! # What reset actually restores
//!
//! Not "the palette's opacity". The bytes a `VoxelGrid` hands over are the
//! plan-view palette's alpha already multiplied by the product's 3D
//! transparency profile (`rustdar_radar::voxel::volume_alpha_profile`), so a
//! value the map paints solid can come back invisible here: the palette's
//! alpha for ρHV at 0.99 is 180, and the grid table's is 0, because uniform
//! precipitation is what that profile exists to see through. The button and
//! its hover say so. What makes the reset safe anyway is that
//! `volume_bridge::effective_lut` *replaces* the alpha channel rather than
//! multiplying into it, so the profile is a recoverable default and not a
//! floor the user can never climb back to — and the canvas below the button
//! draws the table, so the truth is on screen either way.
//!
//! # Index 0
//!
//! The curve's leftmost entry is the no-data index and the editor cannot
//! raise it: `AlphaCurve::from_alphas` clamps it to zero, `apply_stroke`
//! re-clamps after every segment, and the window says so in its footer so a
//! user dragging into the left edge sees intent rather than a glitch.
//! Unmeasured air must never be paintable — that would dress every storm in a
//! shell of fabricated echo.

use crate::volume_alpha::{AlphaCurve, AlphaCurves, CURVE_LEN, apply_stroke};

/// The pane-corner button's label. Named here so the input harness can find
/// the button the same way the menu labels are found.
pub(crate) const ALPHA_BUTTON_LABEL: &str = "Volume alpha";

/// The reset button's label.
///
/// It read "Reset to palette" and the hover said "render through the
/// palette's own opacity again", which stopped being true when the
/// per-product 3D transparency profiles landed after the editor: reset
/// restores the *grid* table, which is the palette's alpha times the profile.
/// For ρHV at 0.99 the palette's own opacity is 180 and what reset gives is 0.
/// Named as a constant so the wording has one home and the test that pins it
/// cannot drift from the button.
pub(crate) const RESET_LABEL: &str = "Reset to the 3D default";

/// Inset of the button from the pane's top-right corner, points. Mirrors the
/// caption's margin in the opposite corner.
const BUTTON_MARGIN: f32 = 8.0;

/// The curve canvas's height, points. Tall enough that one point of pointer
/// travel is under 1% of alpha, so a curve can be placed rather than lurched.
const CURVE_HEIGHT: f32 = 110.0;

/// The palette strip's height, points — a legend, not a control.
const STRIP_HEIGHT: f32 = 14.0;

/// Gap between the curve canvas and the palette strip, points.
const STRIP_GAP: f32 = 4.0;

/// The 3D pane's Volume Alpha surface: the corner button, and the editor
/// window while it is open.
///
/// `target` is the target the pane's own arm just resolved — `None` when the
/// arm never got far enough to name one (no site data yet, no painter). The
/// editor still opens in that state; it shows its waiting text until a grid's
/// palette can be looked up.
#[allow(clippy::too_many_arguments)]
pub(crate) fn editor_ui(
    ui: &mut egui::Ui,
    pane_rect: egui::Rect,
    pane_idx: usize,
    pane: &mut crate::pane::PaneState,
    painter: Option<&dyn crate::volume_view::VolumePainter>,
    target: Option<&crate::pane::VolumeTarget>,
    chrome: Option<f32>,
    curves: &mut AlphaCurves,
    #[cfg(test)] probe: &mut Vec<(usize, egui::Rect)>,
) {
    let product = pane.selected_product;
    let Some(volume) = pane.volume_mut() else {
        return;
    };

    // The UI fade (§1.8): the corner button is floating chrome over the
    // picture, exactly like the pills, so fully faded it does not render at
    // all — the absence is the input transparency — and it returns on the
    // unfade like the rest. The editor window needs no gate of its own: the
    // fade closes it for real (`Gui::fade_close_all`), and an open editor
    // found under a fade unfades the frame (`enforce_fade_invariants`).
    let Some(chrome) = chrome else {
        return;
    };

    // The corner button. Drawn after the pane's own painting, so it sits over
    // the volume; egui resolves overlapping widgets to the later one, so it
    // wins the pointer over the pane-wide orbit interact.
    let button = egui::Button::new(egui::RichText::new(ALPHA_BUTTON_LABEL).size(11.0));
    let size = egui::vec2(88.0, 20.0);
    let rect = egui::Rect::from_min_size(
        pane_rect.right_top() + egui::vec2(-(size.x + BUTTON_MARGIN), BUTTON_MARGIN),
        size,
    );
    #[cfg(test)]
    probe.push((pane_idx, rect));
    let drawn = ui
        .scope(|ui| {
            // A transitioning button dims with the chrome and is dead to
            // input meanwhile — the standing `fade::dim` contract, inlined
            // because this module sits outside the `ui` module tree.
            if chrome < 1.0 {
                ui.multiply_opacity(chrome);
                ui.disable();
            }
            ui.put(rect, button)
        })
        .inner;
    if drawn
        .on_hover_text(
            "Redraw the volume's opacity over the value scale - GR2Analyst's Volume Alpha. \
             Drag on the curve to strip or restore a range of values.",
        )
        .clicked()
    {
        volume.alpha_editor_open = !volume.alpha_editor_open;
    }
    if !volume.alpha_editor_open {
        return;
    }

    // One window per pane, its position remembered under the pane's id while
    // the title tracks the product — so switching moments re-labels the same
    // window instead of scattering one per product across the screen.
    let mut open = true;
    egui::Window::new(format!("Volume Alpha - {}", product.name()))
        .id(egui::Id::new(("volume_alpha_editor", pane_idx)))
        .open(&mut open)
        .default_width(460.0)
        .default_pos(pane_rect.center() - egui::vec2(230.0, 90.0))
        .resizable(true)
        .show(ui.ctx(), |ui| {
            editor_contents(ui, pane_idx, product, painter, target, curves);
        });
    volume.alpha_editor_open = open;
}

/// What the editor says when there is no table to draw a curve over.
///
/// Two different absences, two different sentences. A product the vertical
/// views refuse will *never* have a table here, so telling its user to wait
/// would be the window lying about a permanent state; a product they admit
/// has one on the way.
///
/// The predicate is `derive::volume_slot`, **not** `sampler::samplable`. That
/// difference is the whole of the derived products' admission to 3D: SRV,
/// NROT and KDP have no native moment and would be refused by name here on
/// the narrower predicate, while the volume behind the window rendered them
/// perfectly well. A function rather than an `if` inside the widget so the
/// distinction has a test — it did not, and all three gates that carry it
/// could be reverted to `samplable` with the whole workspace green.
fn absent_curve_message(product: rustdar_radar::types::RadarProduct) -> String {
    if rustdar_radar::derive::volume_slot(product).is_none() {
        format!(
            "{} does not render in 3D, so there is no volume opacity to edit \
             - pick a moment the radar measures or derives tilt by tilt.",
            product.name(),
        )
    } else {
        "The volume is still building - its palette arrives with it, and the \
         curve is drawn over that palette."
            .to_owned()
    }
}

/// The window's body: header row, the curve canvas over the palette strip,
/// and the no-data footnote.
fn editor_contents(
    ui: &mut egui::Ui,
    pane_idx: usize,
    product: rustdar_radar::types::RadarProduct,
    painter: Option<&dyn crate::volume_view::VolumePainter>,
    target: Option<&crate::pane::VolumeTarget>,
    curves: &mut AlphaCurves,
) {
    // The grid's own palette, through the same pane-scoped lookup the render
    // uses — so the strip below the curve is the very table the volume is
    // being drawn through, stand-in grid and all.
    let palette = target.and_then(|t| painter.and_then(|p| p.palette(pane_idx, t)));
    let palette_curve = palette.as_deref().and_then(AlphaCurve::from_palette);

    // What the canvas shows: the user's curve, else the grid table's own
    // alpha — palette times profile, the very bytes the volume is drawn
    // through. An untouched editor therefore *shows* exactly what renders —
    // and stores nothing, which is what keeps it bit-exact.
    let shown = curves.get(product).or_else(|| palette_curve.clone());
    let Some(shown) = shown else {
        ui.label(absent_curve_message(product));
        return;
    };

    ui.horizontal(|ui| {
        if ui
            .add_enabled(curves.is_edited(product), egui::Button::new(RESET_LABEL))
            .on_hover_text(
                "Forget the drawn curve and render through this product's default volume \
                 opacity again - the plan-view palette's alpha shaped by the product's \
                 own 3D transparency profile. That is not the plan view's opacity: a value \
                 the map paints solid can be see-through here, which is what makes a storm's \
                 interior visible.",
            )
            .clicked()
        {
            curves.reset(product);
        }
        if curves.is_edited(product) {
            ui.weak("edited");
        }
    });

    let width = ui.available_width().max(256.0);
    let (response, canvas) = ui.allocate_painter(
        egui::vec2(width, CURVE_HEIGHT + STRIP_GAP + STRIP_HEIGHT),
        egui::Sense::click_and_drag(),
    );
    let curve_rect = egui::Rect::from_min_size(
        response.rect.left_top(),
        egui::vec2(response.rect.width(), CURVE_HEIGHT),
    );
    let strip_rect = egui::Rect::from_min_size(
        curve_rect.left_bottom() + egui::vec2(0.0, STRIP_GAP),
        egui::vec2(response.rect.width(), STRIP_HEIGHT),
    );

    paint_editor(
        &canvas,
        curve_rect,
        strip_rect,
        &shown,
        palette.as_deref(),
        palette_curve.as_ref().filter(|_| curves.is_edited(product)),
    );

    // --- Interaction ------------------------------------------------------
    //
    // The stroke: one segment per frame from the previous pointer sample to
    // this frame's, in curve units. The previous sample lives in egui's
    // per-frame temp storage keyed by this widget — pointer history is a
    // posture of the gesture, not state of the pane.
    let anchor_id = response.id.with("stroke_anchor");
    if response.dragged_by(egui::PointerButton::Primary)
        || response.drag_started_by(egui::PointerButton::Primary)
    {
        if let Some(pos) = response.interact_pointer_pos() {
            let sample = curve_point(curve_rect, pos);
            let previous = ui
                .ctx()
                .data_mut(|d| d.get_temp::<(f32, f32)>(anchor_id))
                .unwrap_or(sample);
            let mut alphas = *shown.alphas();
            apply_stroke(&mut alphas, previous, sample);
            curves.set(product, AlphaCurve::from_alphas(alphas));
            ui.ctx().data_mut(|d| d.insert_temp(anchor_id, sample));
        }
    } else {
        ui.ctx()
            .data_mut(|d| d.remove_temp::<(f32, f32)>(anchor_id));
        // A bare click paints a single index — the smallest possible region.
        if response.clicked()
            && let Some(pos) = response.interact_pointer_pos()
        {
            let sample = curve_point(curve_rect, pos);
            let mut alphas = *shown.alphas();
            apply_stroke(&mut alphas, sample, sample);
            curves.set(product, AlphaCurve::from_alphas(alphas));
        }
    }
    if response.secondary_clicked() {
        curves.reset(product);
    }

    ui.weak(
        "Drag to redraw opacity over a value range; the rest of the curve keeps its shape. \
         Right-click to reset. Index 0 is no-data and always stays transparent.",
    );
}

/// A pointer position as `(index, alpha)` in curve units: index `0..=255`
/// left to right, alpha `0..=1` bottom to top. Clamped, so a drag that runs
/// off the canvas keeps painting at the edge it left through — the GR
/// behaviour, and the one that lets "drag along the bottom" zero a range
/// without pixel-perfect aim.
fn curve_point(curve_rect: egui::Rect, pos: egui::Pos2) -> (f32, f32) {
    let index = if curve_rect.width() > 0.0 {
        ((pos.x - curve_rect.left()) / curve_rect.width() * 255.0).clamp(0.0, 255.0)
    } else {
        0.0
    };
    let alpha = if curve_rect.height() > 0.0 {
        ((curve_rect.bottom() - pos.y) / curve_rect.height()).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (index, alpha)
}

/// Draw the dark canvas, the palette strip, the grid table's own alpha as a
/// reference line while an edit diverges from it, and the shown curve.
fn paint_editor(
    canvas: &egui::Painter,
    curve_rect: egui::Rect,
    strip_rect: egui::Rect,
    shown: &AlphaCurve,
    palette: Option<&[u8]>,
    palette_reference: Option<&AlphaCurve>,
) {
    // The GR look: a near-black window so the white curve and the palette
    // colours carry all the contrast.
    canvas.rect_filled(curve_rect, 2.0, egui::Color32::from_gray(16));
    for quarter in 1..4 {
        let y = curve_rect.bottom() - curve_rect.height() * quarter as f32 / 4.0;
        canvas.hline(
            curve_rect.x_range(),
            y,
            egui::Stroke::new(1.0, egui::Color32::from_gray(38)),
        );
    }

    // The palette strip: one stripe per LUT entry, the product's own colours.
    // Missing palette (a curve remembered from a session whose grid has not
    // rebuilt yet) leaves a neutral bar rather than fabricated colours.
    match palette {
        Some(lut) => {
            let stripe = strip_rect.width() / CURVE_LEN as f32;
            for (i, entry) in lut.chunks_exact(4).enumerate() {
                let left = strip_rect.left() + stripe * i as f32;
                canvas.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(left, strip_rect.top()),
                        egui::pos2(left + stripe + 0.5, strip_rect.bottom()),
                    ),
                    0.0,
                    egui::Color32::from_rgb(entry[0], entry[1], entry[2]),
                );
            }
        }
        None => {
            canvas.rect_filled(strip_rect, 2.0, egui::Color32::from_gray(40));
        }
    }

    let polyline = |curve: &AlphaCurve| -> Vec<egui::Pos2> {
        curve
            .alphas()
            .iter()
            .enumerate()
            .map(|(i, alpha)| {
                egui::pos2(
                    curve_rect.left() + curve_rect.width() * i as f32 / 255.0,
                    curve_rect.bottom() - curve_rect.height() * f32::from(*alpha) / 255.0,
                )
            })
            .collect()
    };

    // The grid table's own alpha as a faint reference under an edited curve, so
    // "how far am I from the default" is visible while dragging.
    if let Some(reference) = palette_reference {
        canvas.add(egui::Shape::line(
            polyline(reference),
            egui::Stroke::new(1.0, egui::Color32::from_gray(90)),
        ));
    }
    canvas.add(egui::Shape::line(
        polyline(shown),
        egui::Stroke::new(1.5, egui::Color32::WHITE),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reset button must not promise the palette's opacity.
    ///
    /// It read "Reset to palette", hover "render through the palette's own
    /// opacity again". That was true when the editor landed and stopped being
    /// true when the per-product 3D transparency profiles landed after it:
    /// what reset restores is `VoxelGrid::lut()`, which is the palette's alpha
    /// **times** the profile. For ρHV the palette's own opacity at 0.99 is 180
    /// and what the button gives is 0 — not merely different, but the
    /// difference between a solid wall and nothing at all, on the one product
    /// whose profile exists to see through uniform rain.
    ///
    /// Pinned as text because the label *is* the artifact: the behaviour was
    /// never wrong, only the sentence describing it, and a sentence has no
    /// other test.
    /// The editor admits the three derived products, and refuses only what
    /// has no per-tilt field at all.
    ///
    /// One of the three UI-facing gates that let SRV, NROT and KDP into the
    /// vertical views, and until now none of them had a test: all three could
    /// be reverted to `sampler::samplable` — the exact pre-admission code —
    /// with every test in the workspace green, leaving every derived pane
    /// refusing by name. The headline feature of the products WP had no
    /// UI-facing pin at all.
    #[test]
    fn the_editor_admits_the_derived_products_and_refuses_only_the_fieldless() {
        use rustdar_radar::types::RadarProduct;
        let refused = |p| absent_curve_message(p).contains("does not render in 3D");
        for product in [
            RadarProduct::StormRelativeVelocity,
            RadarProduct::NormalizedRotation,
            RadarProduct::SpecificDifferentialPhase,
        ] {
            assert!(
                !refused(product),
                "{} is derived tilt by tilt and renders in 3D, but the editor \
                 refuses it by name",
                product.name(),
            );
            assert!(
                rustdar_radar::sampler::samplable(product).is_none(),
                "precondition: {} has no native moment, so this test is about \
                 the `volume_slot` gate and not about `samplable`",
                product.name(),
            );
        }
        // And the products with no per-tilt field at all are still refused,
        // so the gate has not simply been opened.
        for product in [
            RadarProduct::HydrometeorClassification,
            RadarProduct::VerticallyIntegratedLiquid,
            RadarProduct::EchoTops,
            RadarProduct::PrecipitationRate,
        ] {
            assert!(refused(product), "{}", product.name());
        }
    }

    #[test]
    fn the_reset_button_does_not_promise_the_palettes_own_opacity() {
        assert!(
            !RESET_LABEL.to_ascii_lowercase().contains("palette"),
            "the reset button reads {RESET_LABEL:?}, which promises the plan \
             view's opacity and delivers the 3D profile's",
        );
        assert!(
            RESET_LABEL.to_ascii_lowercase().contains("default"),
            "the reset button reads {RESET_LABEL:?}, which does not say what \
             it restores",
        );
    }
}
