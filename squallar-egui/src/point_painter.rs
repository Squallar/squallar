//! Egui implementation of [`squallar_overlays::render::draw::PointPainter`].

use egui::{Color32, FontId, Pos2, Shape, Stroke};
use squallar_overlays::render::draw::{PointPainter, TextAnchor};

pub(crate) struct EguiPointPainter<'a> {
    pub painter: &'a egui::Painter,
    pub center: Pos2,
    /// Whether this layer's GEOMETRY is already drawn somewhere else.
    ///
    /// A layer that rasterizes a picture has had its shapes drawn in the
    /// worker; drawing them again here would paint them twice and pay the
    /// tessellator for the copy that is not visible. Text is the exception and
    /// the reason this is a flag rather than a skipped call: `tiny_skia` has no
    /// fonts, so the picture carries no text and the frame thread is the only
    /// place a galley can be laid out.
    ///
    /// Set from `job_codec(id).is_some()` at the call site, so a layer that
    /// gains a picture stops double-drawing the moment it does, with nothing
    /// to remember.
    pub text_only: bool,
    /// The galley memo every `text` call on this painter goes through.
    ///
    /// A station model is several numbers per station and there are hundreds
    /// of stations on screen, so this path lays out more galleys per frame
    /// than the basemap's place names do. Lent by the pane walk, the same
    /// cache the `CityLabels` arm uses; see [`walkers::GalleyCache`].
    pub galleys: &'a mut walkers::GalleyCache,
}

impl EguiPointPainter<'_> {
    fn pos(&self, offset: [f32; 2]) -> Pos2 {
        Pos2::new(self.center.x + offset[0], self.center.y + offset[1])
    }

    fn color(rgba: [u8; 4]) -> Color32 {
        Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3])
    }
}

impl PointPainter for EguiPointPainter<'_> {
    fn circle_filled(&mut self, offset: [f32; 2], radius: f32, color: [u8; 4]) {
        if self.text_only {
            return;
        }
        self.painter
            .circle_filled(self.pos(offset), radius, Self::color(color));
    }

    fn circle_stroke(&mut self, offset: [f32; 2], radius: f32, color: [u8; 4], width: f32) {
        if self.text_only {
            return;
        }
        self.painter.circle_stroke(
            self.pos(offset),
            radius,
            Stroke::new(width, Self::color(color)),
        );
    }

    fn text(
        &mut self,
        offset: [f32; 2],
        text: &str,
        color: [u8; 4],
        size: f32,
        anchor: TextAnchor,
    ) {
        let align = match anchor {
            TextAnchor::TopLeft => egui::Align2::LEFT_TOP,
            TextAnchor::TopRight => egui::Align2::RIGHT_TOP,
            TextAnchor::BottomLeft => egui::Align2::LEFT_BOTTOM,
            TextAnchor::BottomRight => egui::Align2::RIGHT_BOTTOM,
            TextAnchor::CenterLeft => egui::Align2::LEFT_CENTER,
            TextAnchor::CenterRight => egui::Align2::RIGHT_CENTER,
            TextAnchor::Center => egui::Align2::CENTER_CENTER,
            TextAnchor::CenterTop => egui::Align2::CENTER_TOP,
            TextAnchor::CenterBottom => egui::Align2::CENTER_BOTTOM,
        };
        // `Painter::text` spelled out, with the layout answered from the memo:
        // it is `layout_no_wrap` (which allocates a `String` from `text` and
        // takes `Context::write`), then `Align2::anchor_size`, then `galley`.
        // The placement arithmetic below is that function's, unchanged.
        let color = Self::color(color);
        let galley = self.galleys.galley_for_point(
            self.painter.ctx(),
            text,
            FontId::proportional(size),
            color,
        );
        let rect = align.anchor_size(self.pos(offset), galley.size());
        self.painter.galley(rect.min, galley, color);
    }

    fn line(&mut self, from: [f32; 2], to: [f32; 2], color: [u8; 4], width: f32) {
        if self.text_only {
            return;
        }
        self.painter.line_segment(
            [self.pos(from), self.pos(to)],
            Stroke::new(width, Self::color(color)),
        );
    }

    fn filled_polygon(&mut self, points: &[[f32; 2]], color: [u8; 4]) {
        if self.text_only {
            return;
        }
        if points.len() < 3 {
            return;
        }
        let vertices: Vec<Pos2> = points.iter().map(|p| self.pos(*p)).collect();
        self.painter.add(Shape::convex_polygon(
            vertices,
            Self::color(color),
            Stroke::NONE,
        ));
    }
}

#[cfg(test)]
mod tests {
    /// **The frame thread suppresses a layer's geometry by RULE, not by name.**
    ///
    /// The rule is "a layer that rasterizes a picture has already drawn its
    /// shapes in the worker, so do not draw them again here". Spelling it as
    /// `job_codec(id).is_some()` means the next layer to gain a picture stops
    /// double-drawing the moment it does. Spelling it as "is this METAR" would
    /// be a line that silently rots into a double-draw — geometry painted
    /// twice, once invisibly, with the tessellator billed for both.
    ///
    /// Source-scanned rather than driven, because what is being pinned is the
    /// SHAPE of the condition; a behavioural test would pass just as happily
    /// on the hardcoded spelling this exists to forbid.
    const PANE: &str = include_str!("ui_map_pane.rs");

    #[test]
    fn the_frame_thread_asks_the_registry_whether_a_layer_has_a_picture() {
        assert!(
            PANE.contains("text_only: pf.overlays.job_codec(pf.id).is_some()"),
            "the point painter's `text_only` is no longer set from the \
             registry, so either the geometry suppression is gone or it is \
             hardcoded to one layer",
        );
        assert!(
            !PANE.contains("text_only: pf.id == squallar_source::id::known::METAR"),
            "`text_only` is decided by naming a layer; a second layer with a \
             picture would double-draw and nothing would say so",
        );
    }
}
