//! The Volume Alpha editor: GR2Analyst's drag-editable opacity curve, drawn
//! over the product's own palette strip.

use crate::volume_alpha::{AlphaCurve, AlphaCurves, CURVE_LEN, apply_stroke};

/// The pane-corner button's label. Named here so the input harness can find
/// the button the same way the menu labels are found.
pub(crate) const ALPHA_BUTTON_LABEL: &str = "Volume alpha";

/// The reset button's label.
pub(crate) const RESET_LABEL: &str = "Reset to the 3D default";

/// Inset of the button from the top-right corner it stands in, points. Mirrors
/// the caption's margin in the opposite corner.
const BUTTON_MARGIN: f32 = 8.0;

/// The corner button's size, points — what it has always been drawn at, named
/// now because [`corner_button_rect`] asks whether there is room for it as well
/// as where to put it. Two spellings of one number are two chances for the
/// answers to disagree.
const BUTTON_SIZE: egui::Vec2 = egui::vec2(88.0, 20.0);

/// The curve canvas's height, points. Tall enough that one point of pointer
/// travel is under 1% of alpha, so a curve can be placed rather than lurched.
const CURVE_HEIGHT: f32 = 110.0;

/// The palette strip's height, points — a legend, not a control.
const STRIP_HEIGHT: f32 = 14.0;

/// Gap between the curve canvas and the palette strip, points.
const STRIP_GAP: f32 = 4.0;

/// The 3D pane's Volume Alpha surface: the corner button, and the editor
/// window while it is open.
#[allow(clippy::too_many_arguments)]
pub(crate) fn editor_ui(
    ui: &mut egui::Ui,
    pane_rect: egui::Rect,
    chrome_rect: egui::Rect,
    pane_idx: usize,
    pane: &mut crate::pane::PaneState,
    painter: Option<&dyn crate::volume_view::VolumePainter>,
    target: Option<&crate::pane::VolumeTarget>,
    chrome: Option<f32>,
    curves: &mut AlphaCurves,
    #[cfg(test)] probe: &mut Vec<(usize, egui::Rect)>,
) {
    let product = pane.selected_product();
    let Some(volume) = pane.volume_mut() else {
        return;
    };

    let Some(chrome) = chrome else {
        return;
    };

    if let Some(rect) = corner_button_rect(chrome_rect) {
        let button = egui::Button::new(egui::RichText::new(ALPHA_BUTTON_LABEL).size(11.0));
        #[cfg(test)]
        probe.push((pane_idx, rect));
        let drawn = ui
            .scope(|ui| {
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
    }
    if !volume.alpha_editor_open {
        return;
    }

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

/// Where the corner button stands inside `chrome_rect`, or `None` when that
/// rect has no room for it.
fn corner_button_rect(chrome_rect: egui::Rect) -> Option<egui::Rect> {
    let rect = egui::Rect::from_min_size(
        chrome_rect.right_top() + egui::vec2(-(BUTTON_SIZE.x + BUTTON_MARGIN), BUTTON_MARGIN),
        BUTTON_SIZE,
    );
    chrome_rect.contains_rect(rect).then_some(rect)
}

/// What the editor says when there is no table to draw a curve over.
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
    let palette = target.and_then(|t| painter.and_then(|p| p.palette(pane_idx, t)));
    let palette_curve = palette.as_deref().and_then(AlphaCurve::from_palette);

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
    canvas.rect_filled(curve_rect, 2.0, egui::Color32::from_gray(16));
    for quarter in 1..4 {
        let y = curve_rect.bottom() - curve_rect.height() * quarter as f32 / 4.0;
        canvas.hline(
            curve_rect.x_range(),
            y,
            egui::Stroke::new(1.0, egui::Color32::from_gray(38)),
        );
    }

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
